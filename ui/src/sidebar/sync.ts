// When the app asks for an Agent Standup sync, quietly (nothing is drawn while it runs): once at
// launch, every 10 minutes while the window is visible, on focus when the last sync is more than
// 2 minutes old, and when a board picker opens. Settings → Projects has a Refresh button for the
// rest. Nothing else waits on a sync: new chats and moves use the projects already held.

import type { BoardSyncState } from '../api/projects.ts';

export const POLL_MS = 10 * 60_000;
export const FOCUS_STALE_MS = 2 * 60_000;

/** Opening a picker syncs at most this often; the host runs one sync at a time anyway. */
export const OPEN_SYNC_MIN_MS = 30_000;

/** A function to call when a board picker (or a menu listing boards) opens: it starts `sync`
 *  unless one was started less than `OPEN_SYNC_MIN_MS` ago. `sync` must never throw. */
export function syncOnOpen(sync: () => void, now: () => number = Date.now): () => void {
  let last = -Infinity;
  return () => {
    const t = now();
    if (t - last < OPEN_SYNC_MIN_MS) return;
    last = t;
    sync();
  };
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
