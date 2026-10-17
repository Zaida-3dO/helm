// When the sidebar asks for an Agent Standup sync: once at launch, on Refresh, every 10
// minutes while the window is visible, and on focus when the last sync is more than 2 minutes old.
// Nothing else waits on a sync: new chats and moves use the projects already held.

import type { BoardSyncState } from '../api/projects.ts';

export const POLL_MS = 10 * 60_000;
export const FOCUS_STALE_MS = 2 * 60_000;

export type SyncDisplay = 'unconfigured' | 'syncing' | 'ok' | 'error' | 'never';

export function syncDisplay(s: BoardSyncState | null): SyncDisplay {
  if (!s) return 'never';
  if (s.syncing) return 'syncing';
  if (!s.configured) return 'unconfigured';
  if (s.error) return 'error';
  return s.lastSyncAt ? 'ok' : 'never';
}

/** Whether a focus event should start a sync. */
export function focusWantsSync(s: BoardSyncState | null, now: number): boolean {
  if (!s || !s.configured || s.syncing) return false;
  return s.lastSyncAt === null || now - s.lastSyncAt > FOCUS_STALE_MS;
}

export interface SyncHost {
  addEventListener(type: 'focus', l: () => void): void;
  removeEventListener(type: 'focus', l: () => void): void;
  setInterval(f: () => void, ms: number): unknown;
  clearInterval(id: unknown): void;
}

/** Wire the triggers; returns the cleanup. `sync` must never throw. */
export function startSyncTriggers(opts: {
  sync: () => void;
  state: () => BoardSyncState | null;
  host: SyncHost;
  visible: () => boolean;
  now?: () => number;
}): () => void {
  const now = opts.now ?? Date.now;
  opts.sync();
  const timer = opts.host.setInterval(() => {
    if (opts.visible()) opts.sync();
  }, POLL_MS);
  const onFocus = () => {
    if (focusWantsSync(opts.state(), now())) opts.sync();
  };
  opts.host.addEventListener('focus', onFocus);
  return () => {
    opts.host.clearInterval(timer);
    opts.host.removeEventListener('focus', onFocus);
  };
}
