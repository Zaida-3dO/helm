// Settings → Usage. Tab one: plan usage cards, then Request
// modes. Tab two: feedback threads about Helm. Tab three: what agents said about Helm's features. Every empty state names its reason.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { usageOps, type RequestModes, type UsageSnapshot } from '../../api/usage.ts';
import { useShell } from '../../shell/context.ts';
import { asOf, percentText, RANGES, rangeOf, resetLine, expired, sourceLabel, type RangeId } from '../../usage/model.ts';
import { useUsageStore } from '../../usage/UsageMeter.tsx';
import { Button, PillTabs, Segmented } from '../../ui/index.ts';
import { FeedbackTab } from './FeedbackTab.tsx';
import { HelmFeedbackTab } from './HelmFeedbackTab.tsx';
import '../../usage/usage.css';

type TabId = 'usage' | 'helm-feedback' | 'feedback';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

export function UsagePane() {
  const [tab, setTab] = useState<TabId>('usage');
  return (
    <div className="usage-pane" data-testid="usage-pane">
      <PillTabs<TabId>
        label="Usage views"
        value={tab}
        onChange={setTab}
        testId="usage-tabs"
        tabs={[
          { id: 'usage', label: 'Usage' },
          { id: 'helm-feedback', label: 'Helm feedback' },
          { id: 'feedback', label: 'Agent feedback' },
        ]}
      />
      {tab === 'usage' ? <UsageTab /> : tab === 'helm-feedback' ? <HelmFeedbackTab /> : <FeedbackTab />}
    </div>
  );
}

function Readiness({ snap, error }: { snap: UsageSnapshot | null; error: string | null }) {
  if (error && !snap) {
    return (
      <p className="usage-notice" data-testid="usage-readiness" data-state="inactive">
        Plan usage could not be read from Helm: {error}
      </p>
    );
  }
  if (!snap) return <p className="usage-notice" data-testid="usage-readiness" data-state="inactive">Reading plan usage…</p>;
  const r = snap.readiness;
  // A ready reading with nothing to say needs no notice.
  if (r.state === 'ready' && snap.reading && !snap.reading.stale && r.reason === 'Plan usage is up to date.') return null;
  return (
    <p className="usage-notice" data-testid="usage-readiness" data-state={r.state}>
      {r.reason}
      {r.state === 'ready' && snap.reading?.stale ? ' The numbers below are out of date.' : ''}
    </p>
  );
}

function PlanCards({ snap, now }: { snap: UsageSnapshot | null; now: number }) {
  const r = snap?.reading;
  if (!r) {
    return (
      <p className="usage-notice" data-testid="usage-empty" data-state="inactive">
        No plan usage yet. {snap ? '' : 'Helm is asking Claude Code.'}
      </p>
    );
  }
  const card = (id: string, title: string, w: { pct?: number; resetsAt?: number; at: number; source: 'control' | 'rate_limit_event' }) => {
    const stale = r.stale || expired(w, now);
    return (
      <div className="usage-card" key={id} data-testid={`usage-card-${id}`} data-stale={stale ? 'true' : undefined}>
        <div className="usage-card-title">{title}</div>
        <div className="usage-card-value" data-testid={`usage-card-${id}-pct`}>
          {percentText(w.pct)}
        </div>
        <div className="usage-card-meta">{resetLine(w, now)}</div>
        <div className="usage-card-meta">
          As of {asOf(w.at, now)} · {sourceLabel(w.source)}
        </div>
      </div>
    );
  };
  return (
    <div className="usage-cards" data-testid="usage-cards">
      {r.fiveHour && card('five_hour', '5-hour window', r.fiveHour)}
      {r.weekly && card('weekly', 'Weekly window', r.weekly)}
      {r.perModel.map((m) => card(`model-${m.model}`, `Weekly · ${m.model}`, m))}
    </div>
  );
}

function RequestModesPanel() {
  const shell = useShell();
  const ops = useMemo(() => usageOps(shell.api), [shell.api]);
  const [range, setRange] = useState<RangeId>('today');
  const [modes, setModes] = useState<RequestModes | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(() => {
    let live = true;
    const { since, until } = rangeOf(range, Date.now());
    setLoading(true);
    ops.modes(since, until).then(
      (m) => {
        if (!live) return;
        setModes(m);
        setError(null);
        setLoading(false);
      },
      (e) => {
        if (!live) return;
        setModes(null);
        setError(errorText(e));
        setLoading(false);
      },
    );
    // Changing the range abandons the previous request's answer.
    return () => {
      live = false;
    };
  }, [ops, range]);

  useEffect(() => load(), [load]);

  const total = modes ? modes.fast + modes.standard + modes.unknown : 0;
  const rows: { id: string; label: string; n: number; tip: string }[] = modes
    ? [
        { id: 'fast', label: 'Fast', n: modes.fast, tip: 'Fast, Priority, and Ultrafast' },
        { id: 'standard', label: 'Standard / other', n: modes.standard, tip: 'Standard, default, Flex, and Batch' },
        { id: 'unknown', label: 'Unknown', n: modes.unknown, tip: 'The request did not record a mode' },
      ]
    : [];
  return (
    <section data-testid="request-modes">
      <div className="usage-pane-head">
        <h3>Request modes</h3>
        <Button variant="text" onClick={() => void load()} disabled={loading} data-testid="request-modes-refresh">
          {loading ? 'Counting…' : 'Refresh'}
        </Button>
      </div>
      <Segmented<RangeId> label="Range" testId="request-modes-range" value={range} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} onChange={setRange} />
      {error && (
        <p className="usage-notice" data-testid="request-modes-error" data-state="inactive">
          {error}
        </p>
      )}
      {modes && total === 0 && (
        <p className="usage-notice" data-testid="request-modes-empty" data-state="inactive">
          No requests from Helm’s chats in this range. Only chats started in Helm are counted ({modes.files} transcript{modes.files === 1 ? '' : 's'} read).
        </p>
      )}
      {modes && total > 0 && (
        <div className="usage-modes-bars" data-testid="request-modes-bars">
          {rows.map((r) => (
            <div className="usage-mode-row" key={r.id} title={r.tip} data-testid={`request-mode-${r.id}`}>
              <span>{r.label}</span>
              <span className="usage-mode-track">
                <span className="usage-mode-fill" style={{ width: `${(100 * r.n) / total}%` }} />
              </span>
              <span className="usage-mode-count" data-testid={`request-mode-${r.id}-count`}>
                {r.n.toLocaleString()}
              </span>
            </div>
          ))}
        </div>
      )}
      <p className="usage-fine">Historical request metadata. Current settings do not establish how older requests ran.</p>
    </section>
  );
}

function UsageTab() {
  const { store, snap, error } = useUsageStore();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => setNow(Date.now()), [snap]);
  const waiting = snap?.nextRefreshAt !== undefined && snap.nextRefreshAt > now;
  return (
    <>
      <section data-testid="plan-usage">
        <div className="usage-pane-head">
          <h3>Plan usage</h3>
          <Button
            variant="text"
            onClick={() => void store.refresh()}
            disabled={snap?.refreshing === true}
            title={waiting ? 'Read a moment ago; Refresh asks again after a minute' : undefined}
            data-testid="usage-refresh"
          >
            {snap?.refreshing ? 'Reading…' : 'Refresh'}
          </Button>
        </div>
        <Readiness snap={snap} error={error} />
        <PlanCards snap={snap} now={now} />
        {snap?.reading?.plan && <p className="usage-fine">Plan: {snap.reading.plan}. Numbers come from Claude Code itself; Helm reads no account data.</p>}
      </section>
      <RequestModesPanel />
    </>
  );
}
