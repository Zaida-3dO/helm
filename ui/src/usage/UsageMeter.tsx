// The top-bar plan-usage meter: two compact bars, 5-hour and weekly, with the percentage;
// reset times and the source in the tooltip; greyed when the reading is stale. A click opens
// Settings → Usage. With no reading it says why (the readiness reason) instead of showing nothing.

import { useEffect, useSyncExternalStore } from 'react';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import { requestPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';
import { bars, emptyTitle } from './model.ts';
import { usageStoreFor } from './store.ts';
import './usage.css';

export const USAGE_PANE = 'usage';

export function useUsageStore() {
  const shell = useShell();
  const store = usageStoreFor(shell.api, shell.bus);
  useEffect(() => {
    store.start();
  }, [store]);
  const snap = useSyncExternalStore(store.subscribe, () => store.snapshot);
  const error = useSyncExternalStore(store.subscribe, () => store.error);
  return { store, snap, error };
}

export function UsageMeter() {
  const shell = useShell();
  const { snap, error } = useUsageStore();
  const now = Date.now();
  const list = bars(snap, now);
  const open = () => {
    requestPane(USAGE_PANE);
    shell.openDialog(SETTINGS_DIALOG);
  };
  if (list.length === 0) {
    const why = error ?? emptyTitle(snap);
    return (
      <button type="button" className="usage-meter usage-meter-empty" data-testid="usage-meter" data-state="empty" title={why} aria-label={`Plan usage unavailable: ${why}`} onClick={open}>
        <span className="usage-label">Usage</span>
        <span className="usage-pct">—</span>
      </button>
    );
  }
  return (
    <button
      type="button"
      className="usage-meter"
      data-testid="usage-meter"
      data-state={list.some((b) => b.stale) ? 'stale' : 'live'}
      title={list.map((b) => b.tooltip).join('\n\n')}
      aria-label={`Plan usage: ${list.map((b) => `${b.label} ${b.text}`).join(', ')}. Open Settings, Usage`}
      onClick={open}
    >
      {list.map((b) => (
        <span
          className="usage-item"
          key={b.id}
          data-testid={`usage-bar-${b.id}`}
          data-stale={b.stale ? 'true' : undefined}
          data-unknown={b.pct === null ? 'true' : undefined}
          data-pct={b.pct ?? ''}
        >
          <span className="usage-label">{b.short}</span>
          <span className="usage-track" aria-hidden="true">
            <span className="usage-fill" data-level={level(b.pct)} style={{ width: `${Math.max(0, Math.min(100, b.pct ?? 0))}%` }} />
          </span>
          <span className="usage-pct" data-testid={`usage-pct-${b.id}`}>
            {b.text}
          </span>
        </span>
      ))}
    </button>
  );
}

function level(pct: number | null): 'low' | 'mid' | 'high' | 'unknown' {
  if (pct === null) return 'unknown';
  return pct >= 90 ? 'high' : pct >= 70 ? 'mid' : 'low';
}
