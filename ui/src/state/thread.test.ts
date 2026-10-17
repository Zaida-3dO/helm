import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyChat, applyPage, applyRows, applyThread, emptyView, MAX_ROWS } from './thread.ts';
import { chat, row, thread } from '../dev/fixtures.ts';

test('a row is only replaced by a newer revision', () => {
  let v = emptyView('c1');
  v = applyRows(v, [row({ id: 1, role: 'assistant', text: 'new', revision: 5 })]);
  const same = applyRows(v, [row({ id: 1, role: 'assistant', text: 'old', revision: 5 })]);
  assert.equal(same, v, 'an equal revision is dropped');
  const older = applyRows(v, [row({ id: 1, role: 'assistant', text: 'old', revision: 4 })]);
  assert.equal(older.rows[0]!.text, 'new');
  const newer = applyRows(v, [row({ id: 1, role: 'assistant', text: 'newer', revision: 6 })]);
  assert.equal(newer.rows[0]!.text, 'newer');
});

test('rows are ordered by timestamp, then sequence, then id', () => {
  const v = applyRows(emptyView('c1'), [
    row({ id: 3, role: 'assistant', timestamp: 10, sequence: 2 }),
    row({ id: 1, role: 'user', timestamp: 20, sequence: 1 }),
    row({ id: 2, role: 'assistant', timestamp: 10, sequence: 1 }),
  ]);
  assert.deepEqual(
    v.rows.map((r) => r.id),
    [2, 3, 1],
  );
});

test('rows of another chat are ignored', () => {
  const v = emptyView('c1');
  assert.equal(applyRows(v, [row({ id: 1, role: 'user', chatId: 'c2' })]), v);
});

test('the thread is capped at MAX_ROWS, trimming the end the reader is not on', () => {
  const many = Array.from({ length: MAX_ROWS + 10 }, (_, i) => row({ id: 10_000 + i, role: 'assistant' }));
  const newest = applyRows(emptyView('c1'), many, 'newest');
  assert.equal(newest.rows.length, MAX_ROWS);
  assert.equal(newest.rows.at(-1)?.id, 10_000 + MAX_ROWS + 9);
  assert.equal(newest.hasOlder, true);
  const oldest = applyRows(emptyView('c1'), many, 'oldest');
  assert.equal(oldest.rows.length, MAX_ROWS);
  assert.equal(oldest.rows[0]!.id, 10_000);
  assert.equal(oldest.hasNewer, true);
});

test('stale chat and thread revisions are dropped', () => {
  let v = applyChat(emptyView('c1'), chat({ revision: 3, title: 'a' }));
  assert.equal(applyChat(v, chat({ revision: 3, title: 'b' })).chat?.title, 'a');
  assert.equal(applyChat(v, chat({ revision: 4, title: 'b' })).chat?.title, 'b');
  v = applyThread(v, thread({ revision: 2, queueHeld: true }));
  assert.equal(applyThread(v, thread({ revision: 1 })).thread?.queueHeld, true);
  assert.equal(applyThread(v, thread({ revision: 3 })).thread?.queueHeld, false);
});

test('pages: replace resets, older prepends', () => {
  let v = applyRows(emptyView('c1'), [row({ id: 50, role: 'user' })]);
  const page = {
    chatId: 'c1',
    hasOlder: true,
    hasNewer: false,
    olderCursor: 'o',
    newerCursor: null,
    runs: [],
    pending: [],
    revision: 1,
  };
  v = applyPage(v, { ...page, messages: [row({ id: 60, role: 'user' })] }, 'replace');
  assert.deepEqual(
    v.rows.map((r) => r.id),
    [60],
  );
  v = applyPage(v, { ...page, hasOlder: false, olderCursor: null, messages: [row({ id: 55, role: 'user' })] }, 'older');
  assert.deepEqual(
    v.rows.map((r) => r.id),
    [55, 60],
  );
  assert.equal(v.hasOlder, false);
});
