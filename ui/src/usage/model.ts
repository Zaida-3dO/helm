// What the top-bar meter and Settings → Usage show, as plain data. Kept free of
// React so the rules are tested under node:test.

import type { UsageSnapshot, UsageWindow } from '../api/usage.ts';

export type BarId = 'five_hour' | 'weekly';

export interface BarModel {
  id: BarId;
  label: string;
  short: string;
  /** 0 to 100, or null when the binary gave a reset time and no number. */
  pct: number | null;
  /** "56%", or "?" when there is no number. */
  text: string;
  /** Greyed: the reading is older than 15 minutes, or its window has already reset. */
  stale: boolean;
  tooltip: string;
}

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

/** "in 2d 3h", "in 4h 5m", "in 12m", "now". */
export function relativeFuture(ms: number, now: number): string {
  const d = ms - now;
  if (d <= 0) return 'now';
  if (d >= DAY) return `in ${Math.floor(d / DAY)}d ${Math.floor((d % DAY) / HOUR)}h`;
  if (d >= HOUR) return `in ${Math.floor(d / HOUR)}h ${Math.floor((d % HOUR) / MIN)}m`;
  return `in ${Math.max(1, Math.round(d / MIN))}m`;
}

/** "just now", "5 min ago", "2 h ago", "3 d ago". */
export function asOf(ms: number, now: number): string {
  const d = Math.max(0, now - ms);
  if (d < MIN) return 'just now';
  if (d < HOUR) return `${Math.floor(d / MIN)} min ago`;
  if (d < DAY) return `${Math.floor(d / HOUR)} h ago`;
  return `${Math.floor(d / DAY)} d ago`;
}

function clock(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { weekday: 'short', hour: '2-digit', minute: '2-digit' });
}

export function percentText(pct: number | null | undefined): string {
  if (pct === null || pct === undefined) return '?';
  return `${Math.round(pct)}%`;
}

/** True when the window's reset time has passed: its number belongs to a window that is over. */
export function expired(w: UsageWindow | { resetsAt?: number }, now: number): boolean {
  return typeof w.resetsAt === 'number' && w.resetsAt <= now;
}

export function resetLine(w: { resetsAt?: number }, now: number): string {
  if (typeof w.resetsAt !== 'number') return 'Reset time unknown';
  return expired(w, now) ? 'Window has reset' : `Resets ${clock(w.resetsAt)} (${relativeFuture(w.resetsAt, now)})`;
}

export function sourceLabel(source: 'control' | 'rate_limit_event'): string {
  return source === 'control' ? 'Claude Code usage report' : 'a chat’s rate-limit message';
}

/** The two top-bar bars, or none when there is no reading. */
export function bars(snap: UsageSnapshot | null, now: number): BarModel[] {
  const r = snap?.reading;
  if (!r) return [];
  const one = (id: BarId, label: string, short: string, w: UsageWindow | undefined): BarModel[] => {
    if (!w) return [];
    const stale = r.stale || expired(w, now);
    const tooltip = [`${label}: ${percentText(w.pct)} used`, resetLine(w, now), `Read ${asOf(w.at, now)} from ${sourceLabel(w.source)}`, stale ? 'Out of date' : '']
      .filter(Boolean)
      .join('\n');
    return [{ id, label, short, pct: w.pct ?? null, text: percentText(w.pct), stale, tooltip }];
  };
  return [...one('five_hour', '5-hour window', '5h', r.fiveHour), ...one('weekly', 'Weekly window', 'Wk', r.weekly)];
}

/** The line the meter shows instead of bars when there is no reading: the readiness reason. */
export function emptyTitle(snap: UsageSnapshot | null): string {
  return snap?.readiness.reason ?? 'Reading plan usage…';
}

export type RangeId = 'hour' | 'day' | 'today' | 'week' | 'month' | 'all';

export const RANGES: { id: RangeId; label: string }[] = [
  { id: 'hour', label: 'Past hour' },
  { id: 'day', label: '24 hours' },
  { id: 'today', label: 'Today' },
  { id: 'week', label: '7 days' },
  { id: 'month', label: '30 days' },
  { id: 'all', label: 'All' },
];

/** The `[since, until)` range in epoch milliseconds; "today" starts at local midnight. */
export function rangeOf(id: RangeId, now: number): { since: number; until: number } {
  const until = now;
  switch (id) {
    case 'hour':
      return { since: now - HOUR, until };
    case 'day':
      return { since: now - DAY, until };
    case 'today': {
      const d = new Date(now);
      d.setHours(0, 0, 0, 0);
      return { since: d.getTime(), until };
    }
    case 'week':
      return { since: now - 7 * DAY, until };
    case 'month':
      return { since: now - 30 * DAY, until };
    case 'all':
      return { since: 0, until };
  }
}
