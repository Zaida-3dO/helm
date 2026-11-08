import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { BoardSyncState } from '../api/projects.ts';
import { focusWantsSync, OPEN_SYNC_MIN_MS, POLL_MS, startSyncTriggers, syncOnOpen } from './sync.ts';

const state = (o: Partial<BoardSyncState> = {}): BoardSyncState => ({
  configured: true,
  syncing: false,
  lastSyncAt: 1_000,
  error: null,
  projectCount: 3,
  ...o,
});

test('focus syncs only when the last sync is over 2 minutes old', () => {
  assert.equal(focusWantsSync(state({ lastSyncAt: 0 }), 2 * 60_000), false);
  assert.equal(focusWantsSync(state({ lastSyncAt: 0 }), 2 * 60_000 + 1), true);
  assert.equal(focusWantsSync(state({ lastSyncAt: null }), 5), true);
  assert.equal(focusWantsSync(state({ syncing: true, lastSyncAt: null }), 5), false);
  assert.equal(focusWantsSync(state({ configured: false, lastSyncAt: null }), 5), false);
});

test('triggers: launch, a 10-minute poll only while visible, focus', () => {
  let calls = 0;
  let tick: () => void = () => undefined;
  let interval = 0;
  let focus: () => void = () => undefined;
  let visible = true;
  let now = 0;
  let removed = false;
  const stop = startSyncTriggers({
    sync: () => calls++,
    state: () => state({ lastSyncAt: 0 }),
    visible: () => visible,
    now: () => now,
    host: {
      addEventListener: (_t, l) => (focus = l),
      removeEventListener: () => (removed = true),
      setInterval: (f, ms) => {
        tick = f;
        interval = ms;
        return 1;
      },
      clearInterval: () => undefined,
    },
  });
  assert.equal(calls, 1, 'once at launch');
  assert.equal(interval, POLL_MS);
  tick();
  assert.equal(calls, 2);
  visible = false;
  tick();
  assert.equal(calls, 2, 'no poll while hidden');
  now = 60_000;
  focus();
  assert.equal(calls, 2, 'focus within 2 minutes does nothing');
  now = 3 * 60_000;
  focus();
  assert.equal(calls, 3);
  stop();
  assert.equal(removed, true);
});

test('opening a picker syncs, but not twice within 30 seconds', () => {
  let calls = 0;
  let now = 1_000_000;
  const open = syncOnOpen(() => calls++, () => now);
  open();
  open();
  assert.equal(calls, 1);
  now += OPEN_SYNC_MIN_MS - 1;
  open();
  assert.equal(calls, 1);
  now += 1;
  open();
  assert.equal(calls, 2);
});
