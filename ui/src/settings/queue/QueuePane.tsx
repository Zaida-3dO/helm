// Settings → Queue: how full the machine may run. The local-model working-chat cap (a global
// default and a per-server override), the Claude allowance line where starts hold, and a
// glance at what each server carries right now. The queue itself is worked in the sidebar's
// Queue panel.

import { useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import type { LocalModelsState } from '../../api/generated/localmodels/index.ts';
import type { QueueSummary } from '../../api/generated/index.ts';
import { queueOps } from '../../queue/api.ts';
import { claudeLine, needsSignIn, serverLine, signInLine } from '../../queue/model.ts';
import { SignInAgain } from '../../transcript/SignInAgain.tsx';
import { localModelsOps } from '../localmodels/api.ts';
import { Button, Picker, TextInput, toast } from '../../ui/index.ts';

const DEFAULT_CAP = 3;
const DEFAULT_HOLD_PCT = 95;
const CAP_MAX = 12;

function num(v: unknown, fallback: number): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}

export function QueueSettings() {
  const shell = useShell();
  const api = shell.api;
  const [cap, setCap] = useState<number>(DEFAULT_CAP);
  const [holdPct, setHoldPct] = useState<number>(DEFAULT_HOLD_PCT);
  const [state, setState] = useState<LocalModelsState | null>(null);
  const [summary, setSummary] = useState<QueueSummary | null>(null);

  useEffect(() => {
    void api<unknown>('preference_get', { key: 'localChatCap' }).then((v) => setCap(num(v, DEFAULT_CAP))).catch(() => undefined);
    void api<unknown>('preference_get', { key: 'claudeHoldPct' }).then((v) => setHoldPct(num(v, DEFAULT_HOLD_PCT))).catch(() => undefined);
    void localModelsOps(api).state().then(setState).catch(() => undefined);
    void queueOps(api).list().then(setSummary).catch(() => undefined);
  }, [api]);

  const savePref = (key: string, value: number, undo: () => void) => {
    void api('preference', { key, value }).catch((e: unknown) => {
      undo();
      toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) });
    });
  };

  const saveCap = (serverId: string, chatCap: number | null) => {
    if (!state) return;
    const servers = state.servers.map((s) => ({
      id: s.id,
      name: s.name,
      kind: s.kind,
      baseUrl: s.baseUrl,
      enabled: s.enabled,
      autoStart: s.autoStart,
      ...(s.model != null ? { model: s.model } : {}),
      ...(s.folder != null ? { folder: s.folder } : {}),
      ...(s.preset != null ? { preset: s.preset } : {}),
      ...(chatCap != null ? { chatCap } : {}),
    }));
    void api<LocalModelsState>('local_models_save', { servers })
      .then(setState)
      .catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }));
  };

  const capOptions = (state?.chatCapRange.min ?? 0) <= CAP_MAX
    ? [
        { value: '', title: 'Follow the default' },
        { value: '0', title: 'No cap (any number at once)' },
        ...Array.from({ length: CAP_MAX }, (_, i) => ({ value: String(i + 1), title: `${i + 1} at once` })),
      ]
    : [];

  return (
    <div className="queue-pane" data-testid="queue-settings">
      <p>
        Helm never turns a chat away: when there is no room for a turn to open, it queues the start and opens it when
        a place frees. A chat in the middle of a turn is never interrupted.
      </p>
      <div className="queue-setting" data-testid="queue-cap-default">
        <label htmlFor="queue-cap">Local model chats working at once</label>
        <TextInput
          id="queue-cap"
          type="number"
          inputMode="numeric"
          value={String(cap)}
          onChange={(e) => {
            const n = Math.max(0, Math.min(CAP_MAX, Math.trunc(Number(e.target.value)) || 0));
            setCap(n);
            savePref('localChatCap', n, () => setCap(num(e.target.value, DEFAULT_CAP)));
          }}
        />
        <p>At most this many of your chats work on one local model server at a time; the rest wait at their start. 0 switches the cap off.</p>
      </div>
      <div className="queue-setting" data-testid="queue-hold-pct">
        <label htmlFor="queue-hold">Hold Claude starts at (percent used)</label>
        <TextInput
          id="queue-hold"
          type="number"
          inputMode="numeric"
          value={String(holdPct)}
          onChange={(e) => {
            const n = Math.max(0, Math.min(100, Math.trunc(Number(e.target.value)) || 0));
            setHoldPct(n);
            savePref('claudeHoldPct', n, () => setHoldPct(num(e.target.value, DEFAULT_HOLD_PCT)));
          }}
        />
        <p>A new Claude turn waits once the plan windows read at or over this; it opens by itself when the window resets. 0 switches the hold off.</p>
      </div>
      {state && state.servers.length > 0 && (
        <div data-testid="queue-server-caps">
          <p>A server can set its own cap instead of the default:</p>
          {state.servers.map((s) => (
            <div key={s.id} className="queue-server-cap" data-testid="queue-server-cap" data-server-id={s.id}>
              <span>{s.name}</span>
              <Picker
                value={s.chatCap == null ? '' : String(s.chatCap)}
                options={capOptions}
                onChange={(v) => saveCap(s.id, v === '' ? null : Number(v))}
                label={`${s.name}: how many chats may work at once`}
                title={`${s.name}: how many chats may work at once`}
                testId="queue-server-cap-picker"
              />
            </div>
          ))}
        </div>
      )}
      <div className="queue-setting">
        <span data-testid="queue-summary">
          {summary
            ? [
                ...summary.servers.map((s) => `${s.serverId === 'strata' ? 'Strata' : s.serverId}: ${serverLine(s.working, s.queued)}`),
                claudeLine(summary, Date.now()),
              ].join(' · ')
            : 'Checking what is running…'}
        </span>
        <Button variant="secondary" onClick={() => void queueOps(api).list().then(setSummary).catch(() => undefined)} data-testid="queue-refresh">
          Refresh
        </Button>
      </div>
      {summary != null && summary.signIn.length > 0 && (
        <div className="queue-setting" data-testid="queue-signin">
          {summary.signIn.map((c) => (
            <p key={c.chatId} data-testid="queue-signin-row">
              {c.title}: {signInLine(c)}
            </p>
          ))}
          {needsSignIn(summary) && <SignInAgain message="Claude Code could not refresh its sign-in. Sign in again, then message the chats above." />}
        </div>
      )}
      <p>The queue line itself — who waits, in what order, and what to do about it — is the sidebar's Queue.</p>
    </div>
  );
}
