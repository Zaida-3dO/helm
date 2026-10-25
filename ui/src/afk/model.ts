// Pure helpers for the AFK UI: labels, the wait fields, and the presence throttle.

import type { AfkSource } from './api.ts';

export function sourceLabel(s: AfkSource): string {
  switch (s) {
    case 'manual':
      return 'you';
    case 'screenLock':
      return 'screen locked';
    case 'sleep':
      return 'asleep';
    case 'idle':
      return 'idle';
    case 'unanswered':
      return 'question waiting';
  }
}

export function clockTime(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** A wait field: unticked stores null, re-ticking restores 30. */
export function toggleWait(current: number | null, on: boolean): number | null {
  if (!on) return null;
  return current ?? 30;
}

/** Clamp what the user typed into a range; NaN keeps the fallback. */
export function clampInt(raw: string, lo: number, hi: number, fallback: number): number {
  const n = Math.round(Number(raw));
  if (!Number.isFinite(n) || raw.trim() === '') return fallback;
  return Math.min(hi, Math.max(lo, n));
}

/** Whether the form differs from what the host holds. */
export function dirty<T>(a: T, b: T): boolean {
  return JSON.stringify(a) !== JSON.stringify(b);
}

/**
 * The presence throttle: report focus changes at once, and in-window input at most once per
 * `gapMs` (the host only needs "input since the last 30 s tick").
 */
export class PresenceThrottle {
  private lastInput = -Infinity;
  private readonly gapMs: number;
  constructor(gapMs = 20_000) {
    this.gapMs = gapMs;
  }

  /** True when this input should be reported now. */
  input(now: number): boolean {
    if (now - this.lastInput < this.gapMs) return false;
    this.lastInput = now;
    return true;
  }
}

/** What the composer's AFK pill shows, or null while AFK is not on for every chat. */
export function pillModel(view: { open: { startedAt: number; source: AfkSource } | null; settings: { appliesTo: 'all' | 'chat' } }): { label: string; tooltip: string } | null {
  if (!view.open || view.settings.appliesTo !== 'all') return null;
  const since = clockTime(view.open.startedAt);
  const why = view.open.source === 'manual' ? '' : ` (${sourceLabel(view.open.source)})`;
  return {
    label: `AFK since ${since}`,
    tooltip: `AFK is on${why} since ${since}. Chats are told; questions are answered for you, and permission cards and Allow requests wait until you are back.`,
  };
}
