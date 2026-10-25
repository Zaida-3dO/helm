// Settings → AFK (all-chats mode): enable, the switch, the answer and plan approval,
// the summary prompt, auto-AFK, "When I'm back", the last span's summaries and failed ones with
// Retry / Dismiss, and an explicit Save.

import { useCallback, useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { Button, Checkbox, Segmented, SettingRow, TextArea, TextInput, toast } from '../ui/index.ts';
import { afkOps, type AfkSettings, type AfkSpanDigests, type AfkView } from './api.ts';
import { clampInt, clockTime, dirty, sourceLabel, toggleWait } from './model.ts';
import './afk.css';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

export function AfkPane() {
  const shell = useShell();
  const ops = afkOps(shell.api);
  const [view, setView] = useState<AfkView | null>(null);
  const [form, setForm] = useState<AfkSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const adopt = useCallback((v: AfkView, resetForm: boolean) => {
    setView(v);
    if (resetForm) setForm(v.settings);
  }, []);

  useEffect(() => {
    ops
      .state()
      .then((v) => adopt(v, true))
      .catch((e: unknown) => setError(errorText(e)));
    return shell.bus.on('afk', (e) => adopt(e as unknown as AfkView, false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shell]);

  if (error && !view) return <p className="afk-notice" data-testid="afk-error">AFK settings could not be read: {error}</p>;
  if (!view || !form) return <p className="afk-notice">Reading AFK settings…</p>;

  const patch = (p: Partial<AfkSettings>) => setForm({ ...form, ...p });
  const patchAuto = (p: Partial<AfkSettings['auto']>) => setForm({ ...form, auto: { ...form.auto, ...p } });
  const run = (f: () => Promise<AfkView>, resetForm = false) => {
    setBusy(true);
    f()
      .then((v) => {
        adopt(v, resetForm);
        setError(null);
      })
      .catch((e: unknown) => {
        setError(errorText(e));
        toast.show({ kind: 'error', text: errorText(e) });
      })
      .finally(() => setBusy(false));
  };

  const open = view.open;
  return (
    <div className="afk-pane" data-testid="afk-pane">
      <SettingRow title="AFK mode" description="While you are away, chats are told so, and each chat that worked sends you a “While you were out” summary when you are back." testId="afk-enabled">
        <Checkbox checked={form.enabled} onChange={(on) => patch({ enabled: on })} data-testid="afk-enabled-box" />
      </SettingRow>
      <SettingRow
        title="AFK now"
        description={
          open
            ? `On since ${clockTime(open.startedAt)} (${sourceLabel(open.source)}).`
            : view.settings.enabled
              ? 'Off.'
              : 'Turn AFK mode on and save first.'
        }
        testId="afk-switch"
      >
        <Button
          variant={open ? 'primary' : 'secondary'}
          pressed={!!open}
          disabled={busy || (!open && !view.settings.enabled)}
          onClick={() => run(() => ops.set(!open))}
          data-testid="afk-switch-button"
        >
          {open ? 'End AFK' : 'Start AFK'}
        </Button>
      </SettingRow>

      <h3 className="afk-heading">Answers while away</h3>
      <div className="afk-field">
        <label htmlFor="afk-answer">Automatic answer to questions</label>
        <TextArea
          id="afk-answer"
          data-testid="afk-answer"
          rows={4}
          maxLength={4000}
          placeholder="Built-in: pick reasonable defaults and keep going; set hard choices aside for when you are back."
          value={form.answer ?? ''}
          onChange={(e) => patch({ answer: e.target.value === '' ? null : e.target.value })}
        />
        <div className="afk-hint">Leave blank for the built-in answer. While AFK is on, a chat that asks a question gets this answer, followed by a fixed safety line. Permission cards and Allow requests are never answered: they wait for you and are listed in the summary.</div>
      </div>
      <SettingRow title="Plan approval" description="What happens when an agent asks to leave plan mode while you are away.">
        <Segmented<'defer' | 'approve'>
          label="Plan approval"
          value={form.planApproval}
          options={[
            { value: 'defer', label: 'Wait for me' },
            { value: 'approve', label: 'Approve' },
          ]}
          onChange={(v) => patch({ planApproval: v })}
        />
      </SettingRow>

      <h3 className="afk-heading">Turn on automatically</h3>
      <SettingRow title="When the screen locks" description="Lock must last this many seconds first (0–600).">
        <div className="afk-inline">
          <Checkbox checked={form.auto.screenLock} onChange={(on) => patchAuto({ screenLock: on })} data-testid="afk-lock" />
          <TextInput
            className="afk-num"
            type="number"
            min={0}
            max={600}
            aria-label="Lock grace seconds"
            value={form.auto.lockGraceSeconds}
            onChange={(e) => patchAuto({ lockGraceSeconds: clampInt(e.target.value, 0, 600, form.auto.lockGraceSeconds) })}
          />
        </div>
      </SettingRow>
      <SettingRow title="When the PC goes to sleep">
        <Checkbox checked={form.auto.sleep} onChange={(on) => patchAuto({ sleep: on })} />
      </SettingRow>
      <WaitRow
        title="When a question waits"
        description="Minutes without a reply (5–240)."
        value={form.auto.unansweredMinutes}
        onChange={(v) => patchAuto({ unansweredMinutes: v })}
        testId="afk-unanswered"
      />
      <WaitRow
        title="When idle"
        description="Minutes without keyboard or mouse input (5–240)."
        value={form.auto.idleMinutes}
        onChange={(v) => patchAuto({ idleMinutes: v })}
        testId="afk-idle"
      />
      <SettingRow title="When I'm back" description="Whether coming back ends AFK that turned on by itself. AFK you started yourself only ends when you end it.">
        <Segmented<'onReturn' | 'manual'>
          label="When I'm back"
          value={form.auto.end === 'manual' ? 'manual' : 'onReturn'}
          options={[
            { value: 'onReturn', label: 'End it' },
            { value: 'manual', label: 'Keep it on' },
          ]}
          onChange={(v) => patchAuto({ end: v })}
        />
      </SettingRow>

      <h3 className="afk-heading">“While you were out” summary</h3>
      <div className="afk-field">
        <label htmlFor="afk-digest">Summary prompt</label>
        <TextArea
          id="afk-digest"
          rows={6}
          maxLength={16000}
          placeholder={view.defaults.digestPrompt}
          value={form.digestPrompt ?? ''}
          onChange={(e) => patch({ digestPrompt: e.target.value === '' ? null : e.target.value })}
        />
        <div className="afk-hint">Blank uses the built-in prompt. Placeholders: {'{start}'} {'{end}'} {'{duration}'} {'{deferred}'}.</div>
      </div>

      <div className="afk-save">
        {error && <span className="afk-error" role="alert">{error}</span>}
        <Button variant="secondary" disabled={busy || !dirty(form, view.settings)} onClick={() => setForm(view.settings)}>
          Revert
        </Button>
        <Button variant="primary" disabled={busy || !dirty(form, view.settings)} onClick={() => run(() => ops.save(form), true)} data-testid="afk-save">
          Save
        </Button>
      </div>

      {view.failures && (
        <Summaries
          title="Summaries that could not be sent"
          data={view.failures}
          testId="afk-failures"
          actions={
            <>
              <Button disabled={busy} onClick={() => run(() => ops.retry(view.failures!.span.id))} data-testid="afk-retry">
                Retry
              </Button>
              <Button variant="text" disabled={busy} onClick={() => run(() => ops.dismiss(view.failures!.span.id))}>
                Dismiss
              </Button>
            </>
          }
        />
      )}
      {view.last && <Summaries title="Last time away" data={view.last} testId="afk-last" />}
    </div>
  );
}

function WaitRow(props: { title: string; description: string; value: number | null; onChange: (v: number | null) => void; testId: string }) {
  const { value, onChange } = props;
  return (
    <SettingRow title={props.title} description={props.description} testId={props.testId}>
      <div className="afk-inline">
        <Checkbox checked={value !== null} onChange={(on) => onChange(toggleWait(value, on))} />
        <TextInput
          className="afk-num"
          type="number"
          min={5}
          max={240}
          disabled={value === null}
          aria-label={`${props.title} minutes`}
          value={value ?? 30}
          onChange={(e) => onChange(clampInt(e.target.value, 5, 240, value ?? 30))}
        />
      </div>
    </SettingRow>
  );
}

function Summaries({ title, data, actions, testId }: { title: string; data: AfkSpanDigests; actions?: React.ReactNode; testId: string }) {
  const s = data.span;
  return (
    <section className="afk-summaries" data-testid={testId}>
      <h3 className="afk-heading">{title}</h3>
      <div className="afk-hint">
        {clockTime(s.startedAt)}–{s.endedAt ? clockTime(s.endedAt) : 'now'} · started by {sourceLabel(s.source)}
      </div>
      {data.digests.length === 0 ? (
        <div className="afk-hint">No chat worked while you were away, so no summary was asked for.</div>
      ) : (
        <ul className="afk-list">
          {data.digests.map((d) => (
            <li key={d.chatId} data-state={d.state}>
              <span className="afk-chat">{d.title || 'Untitled chat'}</span>
              <span className="afk-state">{d.state}</span>
              {d.error && <span className="afk-error">{d.error}</span>}
            </li>
          ))}
        </ul>
      )}
      {actions && <div className="afk-save">{actions}</div>}
    </section>
  );
}
