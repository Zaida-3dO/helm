// Settings → Hub's Speakers block: the chats the house's room speakers talk to, the model and
// effort a new one starts with, the board they are shown on, whether they run lean, whether this
// PC takes them, and how long a talk that ended may be named to the next one. It reads and saves
// through the host's speaker ops; the hub line is set above it in the same pane.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LocalModelInfo } from '../../api/types.ts';
import type { ProjectNode } from '../../api/generated/projects/ProjectNode.ts';
import { hubOps, type SpeakerSettings } from '../../api/hub.ts';
import { localModelChoices } from '../../composer/modelEffort.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow, TextArea } from '../../ui/index.ts';
import { SETTINGS_DIALOG } from '../SettingsDialog.tsx';
import { EFFORT_OPTIONS } from '../runtime/model.ts';

type Choice = { value: string; title: string; subtitle?: string };

/** Plain words for the lean tool-set entry names, shown as chip labels. Anything not listed here
 * (a server a person added by name or id) shows under its own saved name. */
const TOOL_LABELS: Record<string, string> = {
  'home assistant': 'Home Assistant',
  memory_search: 'Memory search',
  app_schedule: 'Timers & reminders',
  app_list_chats: 'Past chats · list',
  app_read_chat: 'Past chats · read',
  app_telegram_send: 'Send to phone',
};

const toolLabel = (token: string): string => TOOL_LABELS[token] ?? token;

/** The chips to show: the default order first, then any entry the saved list added. */
function toolTokens(speaker: SpeakerSettings): string[] {
  const out = [...speaker.leanToolsDefault];
  for (const t of speaker.leanTools) if (!out.includes(t)) out.push(t);
  return out;
}

const PREVIOUS_MIN = 0;
const PREVIOUS_MAX = 72;
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

/** The app's own choice first, then each board; a saved board the list lacks still shows. */
function boardChoices(boards: { id: string; name: string }[], saved: string): Choice[] {
  const out: Choice[] = [
    { value: '', title: 'App default', subtitle: 'Where any other new chat in this folder is shown' },
    ...boards.map((b) => ({ value: b.id, title: b.name || b.id })),
  ];
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
  const [quickRoll, setQuickRoll] = useState('');
  const [deepRoll, setDeepRoll] = useState('');
  const [leanPrompt, setLeanPrompt] = useState('');
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [boards, setBoards] = useState<{ id: string; name: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = useCallback((v: SpeakerSettings) => {
    setSpeaker(v);
    setPrevious(String(v.previousHours));
    setQuickRoll(String(v.quickRollAt));
    setDeepRoll(String(v.deepRollAt));
    setLeanPrompt(v.leanPrompt);
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
    void shell
      .api<ProjectNode[]>('project_list', {})
      .then((p) => live && setBoards(p.filter((x) => x.kind === 'board').map((x) => ({ id: x.id, name: x.name }))))
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

  const save = (patch: Partial<SpeakerSettings> & { leanToolsEdited?: boolean }) => {
    if (speaker && profileId) void run(ops.saveSpeaker({ ...speaker, ...patch, profileId }));
  };

  // Adding a tool by typing: any name a person's MCP server is saved under, or any host tool.
  const [newTool, setNewTool] = useState('');
  const addTool = () => {
    if (!speaker) return;
    const token = newTool.trim();
    if (!token || toolTokens(speaker).some((t) => t.toLowerCase() === token.toLowerCase())) return;
    setNewTool('');
    save({ leanTools: [...speaker.leanTools, token], leanToolsEdited: true });
  };

  const hours = Number(previous);
  const previousValid = Number.isInteger(hours) && hours >= PREVIOUS_MIN && hours <= PREVIOUS_MAX;
  const previousChanged = speaker !== null && hours !== speaker.previousHours;
  const modelOptions = useMemo(() => modelChoices(models, speaker?.model ?? ''), [models, speaker?.model]);
  const boardOptions = useMemo(() => boardChoices(boards, speaker?.boardId ?? ''), [boards, speaker?.boardId]);

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
          title="Model for quick answers"
          description="A turn marked quick opens its talk's chat on this model — a small local one answers fastest. Leave it unset and quick turns open like deep ones. A later deep turn moves that chat up to the model above."
        >
          <Picker
            settings
            label="Model for quick answers"
            testId="hub-speaker-quick-model"
            value={speaker.quickModel}
            options={modelChoices(models, speaker.quickModel)}
            searchable
            disabled={busy}
            onChange={(v) => save({ quickModel: v })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          title="Effort for quick answers"
          description="How hard the quick model thinks. Medium keeps thinking on while staying fast."
        >
          <Picker
            settings
            label="Effort for quick answers"
            testId="hub-speaker-quick-effort"
            value={speaker.quickEffort}
            options={effortChoices(speaker.quickEffort)}
            disabled={busy}
            onChange={(v) => save({ quickEffort: v })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          title="Roll a talk's chat over at"
          description="When the next turn arrives to a chat this grown, it continues in a fresh one with a short carry-over note — no model turn, nothing lost. One size for each lane; 0 leaves it off."
        >
          <label className="hub-limit">
            Quick
            <input
              type="number"
              min={0}
              max={ROLL_MAX}
              step={1000}
              aria-label="Quick lane roll-over size"
              data-testid="hub-speaker-quick-roll"
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
              data-testid="hub-speaker-deep-roll"
              value={deepRoll}
              onChange={(e) => setDeepRoll(e.target.value)}
            />
          </label>
          <Button
            variant="secondary"
            disabled={busy || !rollValid(quickRoll) || !rollValid(deepRoll) || (Number(quickRoll) === speaker.quickRollAt && Number(deepRoll) === speaker.deepRollAt)}
            onClick={() => save({ quickRollAt: Number(quickRoll), deepRollAt: Number(deepRoll) })}
          >
            Save
          </Button>
        </SettingRow>
      )}
      {speaker && (!rollValid(quickRoll) || !rollValid(deepRoll)) && (
        <div className="ui-field-error">A roll-over size must be {ROLL_MIN} to {ROLL_MAX.toLocaleString()} tokens, or 0 to leave it off.</div>
      )}
      {speaker && speaker.kept.length > 0 && (
        <SettingRow
          testId="hub-speaker-kept"
          title="Kept after their talk"
          description="These chats ended a talk with something still in them. Each puts itself away once nothing holds it and it has been quiet for half an hour — unless you write in it first."
        >
          {speaker.kept.map((k) => (
            <Button
              key={k.chatId}
              variant="secondary"
              disabled={busy}
              onClick={() => {
                shell.select(k.chatId);
                shell.closeDialog(SETTINGS_DIALOG);
              }}
            >
              Open {k.title}
            </Button>
          ))}
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          title="Board for chats that arrive from the hub"
          description="Where a new room-speaker, phone-voice or Telegram chat is shown, so what the house talked about with nobody at the screen can be found. One board for all three. App default leaves it to the same rules as any other new chat."
        >
          <Picker
            settings
            label="Board for new speaker chats"
            testId="hub-speaker-board"
            value={speaker.boardId}
            options={boardOptions}
            searchable
            disabled={busy}
            onChange={(v) => save({ boardId: v })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          testId="hub-speaker-lean"
          title="Lean speaker chats"
          description="A new room-speaker chat gets a short prompt of its own and few tools: faster for a small model; the chat can do less. The prompt and tool set below shape it. Chats already open keep how they started."
        >
          <Checkbox
            checked={speaker.lean}
            disabled={busy}
            aria-label="Lean speaker chats"
            data-testid="hub-speaker-lean-box"
            onChange={(on) => save({ lean: on })}
          />
        </SettingRow>
      )}
      {speaker && speaker.lean && (
        <SettingRow
          testId="hub-speaker-tools"
          title="Tools a lean chat may use"
          description={`What the new room-speaker chats start with. ${
            speaker.leanToolsCustom
              ? 'This is your saved list; editing it changes only this person.'
              : 'The built-in set so far — toggling a chip or adding one saves a list of your own.'
          } Everything else stays available on demand: the chat can find and call more of Helm's tools by itself.`}
        >
          <div className="hub-chips" data-testid="hub-speaker-tools-chips">
            {toolTokens(speaker).map((token) => (
              <Button
                key={token}
                variant={speaker.leanTools.includes(token) ? 'primary' : 'secondary'}
                disabled={busy}
                aria-pressed={speaker.leanTools.includes(token)}
                data-testid={`hub-speaker-tool-${token.replace(/[^a-zA-Z0-9]/g, '-')}`}
                onClick={() => {
                  const on = speaker.leanTools.includes(token);
                  const next = toolTokens(speaker)
                    .filter((t) => (t === token ? !on : speaker.leanTools.includes(t)))
                    .filter((t, i, all) => all.indexOf(t) === i);
                  save({ leanTools: next, leanToolsEdited: true });
                }}
              >
                {toolLabel(token)}
              </Button>
            ))}
          </div>
          <label className="hub-limit">
            <input
              type="text"
              aria-label="Add a tool to the lean set"
              data-testid="hub-speaker-tool-add-name"
              placeholder="another tool or server name"
              value={newTool}
              disabled={busy}
              onChange={(e) => setNewTool(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') addTool();
              }}
            />
          </label>
          <Button
            variant="secondary"
            disabled={busy || !newTool.trim() || toolTokens(speaker).some((t) => t.toLowerCase() === newTool.trim().toLowerCase())}
            data-testid="hub-speaker-tool-add"
            onClick={addTool}
          >
            Add
          </Button>
        </SettingRow>
      )}
      {speaker && speaker.lean && (
        <SettingRow
          testId="hub-speaker-prompt-row"
          title="Prompt for lean chats"
          description="The whole system prompt a new room-speaker chat is given — spoken style, and what the speaker is for. Leave it empty to use the built-in prompt."
        >
          <TextArea
            rows={5}
            aria-label="Lean chats prompt"
            data-testid="hub-speaker-prompt"
            value={leanPrompt}
            placeholder={speaker.leanPromptDefault}
            disabled={busy}
            onChange={(e) => setLeanPrompt(e.target.value)}
          />
          <Button
            variant="secondary"
            disabled={busy || leanPrompt.trim() === speaker.leanPrompt.trim()}
            data-testid="hub-speaker-prompt-save"
            onClick={() => save({ leanPrompt: leanPrompt.trim() })}
          >
            Save
          </Button>
          <Button
            variant="text"
            disabled={busy || speaker.leanPrompt === ''}
            data-testid="hub-speaker-prompt-reset"
            onClick={() => {
              setLeanPrompt('');
              save({ leanPrompt: '' });
            }}
          >
            Use the built-in prompt
          </Button>
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
