import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import { AttentionStore } from './attention.ts';

const s = (revision: number, unread: string[] = []) => ({
  badge: unread.length,
  busy: 0,
  needsYou: [],
  running: [],
  stalled: [],
  unread: unread.map((chatId) => ({ chatId, title: 't', project: 'p', kind: 'unread' as const, reason: 'Marked unread' })),
  revision,
});

test('the newest revision wins and an older one is dropped', async () => {
  const bus = new EventBus();
  const store = new AttentionStore();
  let calls = 0;
  store.subscribe(() => calls++);
  const api = (async () => s(5, ['a'])) as never;
  store.connect(bus, api);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.get().revision, 5);
  assert.ok(store.isUnread('a'));
  bus.dispatch({ kind: 'attention', summary: s(4, ['b']) });
  assert.equal(store.get().revision, 5, 'older ignored');
  bus.dispatch({ kind: 'attention', summary: s(7) });
  assert.equal(store.isUnread('a'), false, 'read: the toast withdraws');
  assert.equal(calls, 2);
  const seen: string[] = [];
  store.onCompletion((c) => seen.push(c.id));
  bus.dispatch({ kind: 'completion', completion: { id: 'k1' } });
  assert.deepEqual(seen, ['k1']);
});
