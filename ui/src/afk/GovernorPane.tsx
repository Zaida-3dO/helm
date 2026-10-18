// Settings → Usage governor: the band plan usage is in now, the thresholds that
// start each band, and what happens at the limit. Explicit Save.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { Button, Checkbox, Segmented, SettingRow, TextInput, toast } from '../ui/index.ts';
import { bandLabel, governorOps, thresholdsError, type GovernorSettings, type GovernorView } from './governor.ts';
import { clampInt, clockTime, dirty } from './model.ts';
import './afk.css';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

export function GovernorPane() {
  const shell = useShell();
  const [view, setView] = useState<GovernorView | null>(null);
  const [form, setForm] = useState<GovernorSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const ops = governorOps(shell.api);
    ops
      .state()
      .then((v) => {
        setView(v);
        setForm(v.settings);
      })
      .catch((e: unknown) => setError(errorText(e)));
    return shell.bus.on('governor', (e) => setView(e as unknown as GovernorView));
  }, [shell]);

  if (error && !view) return <p className="afk-notice" data-testid="governor-error">The usage governor could not be read: {error}</p>;
  if (!view || !form) return <p className="afk-notice">Reading the usage governor…</p>;

  const invalid = thresholdsError(form);
  const pct = (key: 'selectivePct' | 'windDownPct' | 'stopPct', title: string, description: string) => (
    <SettingRow title={title} description={description}>
      <div className="afk-inline">
        <TextInput
          className="afk-num"
          type="number"
          min={1}
          max={100}
          aria-label={title}
          value={form[key]}
          onChange={(e) => setForm({ ...form, [key]: clampInt(e.target.value, 1, 100, form[key]) })}
        />
        <span className="afk-hint">%</span>
      </div>
    </SettingRow>
  );
  const save = () => {
    setBusy(true);
    governorOps(shell.api)
      .save(form)
      .then((v) => {
        setView(v);
        setForm(v.settings);
        setError(null);
      })
      .catch((e: unknown) => {
        setError(errorText(e));
        toast.show({ kind: 'error', text: errorText(e) });
      })
      .finally(() => setBusy(false));
  };

  return (
    <div className="afk-pane" data-testid="governor-pane">
      <p className="afk-notice" data-testid="governor-band" data-band={view.band}>
        {view.settings.enabled
          ? `Now: ${bandLabel(view.band)}${view.usedPct !== null && view.usedPct !== undefined ? ` (${Math.round(view.usedPct)}% used)` : ''}${view.since ? ` since ${clockTime(view.since)}` : ''}.`
          : 'The governor is off.'}
        {view.told > 0 ? ` ${view.told} chat(s) told to wrap up.` : ''}
        {view.stopped > 0 ? ` ${view.stopped} chat(s) stopped.` : ''}
      </p>
      <SettingRow title="Usage governor" description="Watches your plan usage (the higher of the 5-hour and weekly windows) and slows agents down near the limit. It acts only on a fresh reading." testId="governor-enabled">
        <Checkbox checked={form.enabled} onChange={(on) => setForm({ ...form, enabled: on })} data-testid="governor-enabled-box" />
      </SettingRow>
      {pct('selectivePct', 'Selective from', 'Only your own work and replies should start. (Automated sends will wait once the run-slot queue lands.)')}
      {pct('windDownPct', 'Wind down from', 'Running agents are told once to finish their step, save their work and stop starting new work.')}
      {pct('stopPct', 'Stop at', 'Running agents are stopped once. A chat you start again yourself is left alone.')}
      <SettingRow title="At the limit" description="Stop running agents, or only tell them to wrap up.">
        <Segmented<'stop' | 'wrapUp'>
          label="At the limit"
          value={form.atStop}
          options={[
            { value: 'stop', label: 'Stop them' },
            { value: 'wrapUp', label: 'Wrap up only' },
          ]}
          onChange={(v) => setForm({ ...form, atStop: v })}
        />
      </SettingRow>
      <div className="afk-save">
        {(invalid ?? error) && (
          <span className="afk-error" role="alert">
            {invalid ?? error}
          </span>
        )}
        <Button variant="secondary" disabled={busy || !dirty(form, view.settings)} onClick={() => setForm(view.settings)}>
          Revert
        </Button>
        <Button variant="primary" disabled={busy || !!invalid || !dirty(form, view.settings)} onClick={save} data-testid="governor-save">
          Save
        </Button>
      </div>
    </div>
  );
}
