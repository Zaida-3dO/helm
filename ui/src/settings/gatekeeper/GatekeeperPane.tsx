// Settings → Gatekeeper: the one switch, how often the gate comes to you, its policy words and
// model, and the History of everything it judged — with the review list's marks visible there.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, PillTabs, TextArea, TextInput } from '../../ui/index.ts';
import { gateOps } from '../../gatekeeper/api.ts';
import { agoText, barNote, channelLabel, detailRows, reviewLine, verdictLabel } from '../../gatekeeper/model.ts';
import type { GateAsk, GateHistoryFilter, GateSettingsEdit, GateSettingsView } from '../../gatekeeper/types.ts';
import { workmodeOps } from '../../workmode/api.ts';
import { modeLabel } from '../../workmode/model.ts';
import type { WorkMode, WorkModeView } from '../../api/generated/gatekeeper/index.ts';
import '../../gatekeeper/gatekeeper.css';

const FILTERS: Array<{ id: GateHistoryFilter; label: string }> = [
  { id: 'all', label: 'All' },
  { id: 'decided', label: 'Decided' },
  { id: 'asked', label: 'Asked you' },
  { id: 'failed', label: "Couldn't check" },
];

function errText(e: unknown): string {
  return String((e as Error).message ?? e);
}

function GatekeeperPane() {
  const shell = useShell();
  const ops = useMemo(() => gateOps(shell.api), [shell]);

  // The settings as saved, and as typed.
  const [saved, setSaved] = useState<GateSettingsView | null>(null);
  const [enabled, setEnabled] = useState(true);
  const [frequency, setFrequency] = useState('balanced');
  const [policyText, setPolicyText] = useState('');
  const [model, setModel] = useState('sonnet');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const apply = (v: GateSettingsView) => {
      if (!live) return;
      setSaved(v);
      setEnabled(v.enabled);
      setFrequency(v.askFrequency);
      setPolicyText(v.policy ?? '');
      setModel(v.model);
    };
    const off = shell.bus.on('gate', () => void ops.settingsGet().then(apply).catch(() => undefined));
    ops.settingsGet().then(apply).catch((e) => live && setError(errText(e)));
    return () => {
      live = false;
      off();
    };
  }, [ops, shell]);

  // The bar shown under the frequency pick follows the pick being typed, not the saved one.
  const bars: Record<string, number> = { less: 0.6, balanced: 0.75, more: 0.9 };
  const note = barNote({ askFrequency: frequency, bar: bars[frequency] ?? 0.75 });
  const changed =
    saved !== null &&
    (enabled !== saved.enabled || frequency !== saved.askFrequency || policyText.trim() !== (saved.policy ?? '').trim() || model.trim() !== saved.model);
  const save = () => {
    if (!saved) return;
    const next: GateSettingsEdit = {
      enabled,
      askFrequency: frequency,
      policy: policyText.trim() ? policyText : null,
      model: model.trim(),
    };
    ops
      .settingsSave(next)
      .then((v) => {
        setSaved(v);
        setError(null);
      })
      .catch((e) => setError(errText(e)));
  };

  // Working mode: the app default and the boards that have one of their own. The top-bar chip
  // carries the same choices; this block adds the full list, each override with Clear.
  const wops = useMemo(() => workmodeOps(shell.api), [shell.api]);
  const [modeView, setModeView] = useState<WorkModeView | null>(null);
  useEffect(() => {
    let live = true;
    const read = () =>
      void wops
        .get()
        .then((v) => live && setModeView(v))
        .catch(() => undefined);
    read();
    const off = shell.bus.on('workmode', read);
    return () => {
      live = false;
      off();
    };
  }, [wops, shell]);
  const setMode = (scope: 'app' | 'board', mode: WorkMode | null, boardId?: string) => {
    wops
      .set({ scope, mode, ...(boardId ? { boardId } : {}) })
      .then(setModeView)
      .catch((e) => setError(errText(e)));
  };

  // History: filter, search, and one open detail at a time.
  const [filter, setFilter] = useState<GateHistoryFilter>('all');
  const [query, setQuery] = useState('');
  const [rows, setRows] = useState<GateAsk[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const load = useCallback(() => {
    void ops
      .list({ filter, ...(query.trim() ? { query: query.trim() } : {}), limit: 200 })
      .then(setRows)
      .catch(() => setRows([]));
  }, [ops, filter, query]);
  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="gk-body" data-testid="settings-gatekeeper">
      <p className="gk-note">
        Helm holds each question an agent wants to ask you for a moment and judges it. Where a safe default exists and the answer stays easy to undo, Helm
        takes it and carries on, and you can change your mind later from the Needs attention list or below. Anything that cannot be undone, costs money, sends
        something to other people, touches credentials or touches anything live always comes to you. A question Helm cannot judge comes to you as a normal one.
      </p>
      {saved === null && !error && <p className="gk-note">Reading your settings…</p>}
      {error && <p className="gk-error" data-testid="gate-error">{error}</p>}
      <section className="gk-mode" data-testid="gate-working-mode">
        <PillTabs<WorkMode>
          label="Default for every chat"
          value={modeView?.app ?? 'check_in'}
          onChange={(id) => setMode('app', id)}
          testId="gate-mode-app"
          tabs={[
            { id: 'check_in', label: 'Check in with me' },
            { id: 'run', label: 'Run to completion' },
          ]}
        />
        <p className="gk-foot">Run to completion: chats carry on and decide the small things themselves, keeping each choice for you to review; Helm checks every stop. Check in with me: a chat stops and asks when a choice matters.</p>
        {modeView !== null && modeView.overrides.length > 0 && (
          <ul className="gk-mode-overrides" data-testid="gate-mode-overrides">
            {modeView.overrides.map((b) => (
              <li key={b.id} className="gk-mode-override" data-board-id={b.id}>
                <span>
                  {b.name}: {b.mode ? modeLabel(b.mode) : 'the app’s'}
                </span>
                <Button variant="secondary" data-testid="gate-mode-clear" data-board-id={b.id} onClick={() => setMode('board', null, b.id)}>
                  Clear
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
      {saved !== null && (
        <div className="gk-fields" data-testid="gate-fields">
          <Checkbox label="Answer safe questions for me" checked={enabled} onChange={setEnabled} data-testid="gate-enabled" />
          {saved.paused && (
            <p className="gk-paused" data-testid="gate-paused">
              Helm paused its answers after repeated failures{' '}
              {saved.pausedUntil ? `until ${new Date(saved.pausedUntil).toLocaleString()}` : 'for now'}; questions come to you as normal ones meanwhile.
            </p>
          )}
          <PillTabs<string>
            label="How often to ask you"
            value={frequency}
            onChange={setFrequency}
            testId="gate-frequency"
            tabs={[
              { id: 'less', label: 'Less' },
              { id: 'balanced', label: 'Balanced' },
              { id: 'more', label: 'More' },
            ]}
          />
          <p className="gk-foot" data-testid="gate-bar-note">{note}</p>
          <label className="gk-field">
            <span>Model Helm judges with</span>
            <TextInput value={model} onChange={(e) => setModel(e.target.value)} maxLength={120} data-testid="gate-model" />
          </label>
          <label className="gk-field">
            <span>Your policy words</span>
            <TextArea
              rows={6}
              maxLength={4000}
              value={policyText}
              placeholder={saved.defaultPolicy}
              onChange={(e) => setPolicyText(e.target.value)}
              data-testid="gate-policy"
            />
          </label>
          <p className="gk-foot">Leave the policy blank to use the built-in words above it. The built-in words never name your services; write your own copy if you want them to.</p>
          <div className="gk-save-row">
            <Button variant="primary" disabled={!changed} onClick={save} data-testid="gate-save">
              Save
            </Button>
          </div>
        </div>
      )}
      <section className="gk-history">
        <div className="gk-history-head">
          <PillTabs<GateHistoryFilter>
            label="Filter history"
            value={filter}
            onChange={(f) => {
              setFilter(f);
              setOpen(null);
            }}
            testId="gate-history-filter"
            tabs={FILTERS}
          />
          <TextInput className="gk-history-search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search questions…" data-testid="gate-history-search" />
        </div>
        {rows.length === 0 && <p className="gk-empty" data-testid="gate-history-empty">Nothing here yet. Every question the gate judges leaves a line here for 180 days.</p>}
        {rows.map((r) => (
          <div key={r.id}>
            <div className="gk-row" data-testid="gate-history-row" data-ask-id={r.id} data-verdict={r.verdict}>
              <button type="button" className="gk-row-question" onClick={() => setOpen((o) => (o === r.id ? null : r.id))} aria-expanded={open === r.id}>
                <span className="gk-row-line">{r.verdict === 'decided' ? reviewLine(r) : `${channelLabel(r.channel)}: ${r.question}`}</span>
                <span className="gk-row-where">
                  {r.chatTitle || 'Untitled'} · {agoText(Date.now(), r.askedAt)}
                </span>
              </button>
              <span className="gk-chip">{verdictLabel(r.verdict)}</span>
            </div>
            {open === r.id && (
              <div className="gk-detail" data-testid="gate-history-detail">
                {r.brief && <p className="gk-detail-brief">{r.brief.problem}</p>}
                {detailRows(r).map((d, n) => (
                  <div key={n} className="gk-detail-row">
                    <span className="gk-detail-label">{d.label}</span>
                    <span className="gk-detail-value">{d.value}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
      </section>
      <p className="gk-foot">
        Every judgement is recorded here, and nothing is ever answered through your own consent words. Changing an answer sends the chat a correction from you,
        which it works through the same review as any other change.
      </p>
    </div>
  );
}

export { GatekeeperPane };
