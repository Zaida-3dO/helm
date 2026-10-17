import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventBus } from '../api/events.ts';
import { ChatStore, MAX_THREADS } from './store.ts';
import { chat, row, submission, thread } from '../dev/fixtures.ts';

test('host events reach the store, and stale ones are dropped', () => {
  const bus = new EventBus();
  const store = new ChatStore();
  store.connect(bus);
  let notified = 0;
  store.subscribe(() => notified++);

  bus.dispatch({ kind: 'message', chatId: 'c1', revision: 2, rows: [row({ id: 1, role: 'assistant', text: 'hi', revision: 2 })] });
  bus.dispatch({ kind: 'message', chatId: 'c1', revision: 1, rows: [row({ id: 1, role: 'assistant', text: 'h', revision: 1 })] });
  assert.equal(store.view('c1').rows[0]!.text, 'hi');

  bus.dispatch({ kind: 'thread', chatId: 'c1', thread: thread({ revision: 5, status: 'running' }) });
  bus.dispatch({ kind: 'thread', chatId: 'c1', thread: thread({ revision: 4, status: 'idle' }) });
  assert.equal(store.view('c1').thread?.status, 'running');

  bus.dispatch({ kind: 'queue', chatId: 'c1', revision: 7, pending: [submission({ id: 's' })] });
  bus.dispatch({ kind: 'queue', chatId: 'c1', revision: 6, pending: [] });
  assert.equal(store.view('c1').pending.length, 1, 'a stale queue event is dropped');
  bus.dispatch({ kind: 'queue', chatId: 'c1', revision: 8, pending: [] });
  assert.deepEqual(store.view('c1').pending, []);

  bus.dispatch({ kind: 'chat', chat: chat({ revision: 2, title: 'T' }) });
  assert.equal(store.chatList()[0]!.title, 'T');
  bus.dispatch({ kind: 'refresh', chatId: 'c1' });
  assert.equal(store.view('c1').refreshes, 1);
  assert.ok(notified >= 5);
});

test('unwatched threads are evicted past MAX_THREADS; watched ones stay', () => {
  const store = new ChatStore();
  const unwatch = store.watch('keep');
  store.rows('keep', [row({ id: 1, role: 'user', chatId: 'keep' })]);
  for (let i = 0; i < MAX_THREADS + 3; i++) store.rows(`c${i}`, [row({ id: 100 + i, role: 'user', chatId: `c${i}` })]);
  assert.equal(store.view('keep').rows.length, 1);
  assert.equal(store.view('c0').rows.length, 0, 'the oldest unwatched thread went');
  assert.equal(store.view(`c${MAX_THREADS + 2}`).rows.length, 1);
  unwatch();
});

test('archived chats leave the list and a deleted chat is forgotten', () => {
  const s = new ChatStore();
  const bus = new EventBus();
  s.connect(bus);
  bus.dispatch({ kind: 'chat', chat: chat({ id: 'a', revision: 1 }) });
  bus.dispatch({ kind: 'chat', chat: chat({ id: 'b', revision: 1 }) });
  assert.deepEqual(s.chatList().map((c) => c.id), ['b', 'a']);
  bus.dispatch({ kind: 'chat', chat: chat({ id: 'a', revision: 2, archived: true }) });
  assert.deepEqual(s.chatList().map((c) => c.id), ['b']);
  bus.dispatch({ kind: 'chat_removed', chatId: 'b' });
  assert.deepEqual(s.chatList(), []);
  assert.equal(s.view('b').chat, null);
});
