import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { TurnChanges } from '../../api/panel.ts';
import { TurnLoader } from './loader.ts';

const card = (messageId: number): TurnChanges => ({ messageId, files: [], truncated: false });

test('asks are batched into one request per chat and cached per revision', async () => {
  const calls: number[][] = [];
  const loader = new TurnLoader(async (_chat, ids) => {
    calls.push(ids);
    return ids.filter((i) => i % 2 === 0).map(card);
  }, 5);
  await Promise.all([loader.load('c', 1, 1), loader.load('c', 2, 1), loader.load('c', 4, 1)]);
  assert.deepEqual(calls, [[1, 2, 4]]);
  assert.deepEqual(loader.peek('c', 2, 1), card(2));
  assert.equal(loader.peek('c', 1, 1), null, 'a row with no card is cached as null');
  await loader.load('c', 2, 1);
  assert.equal(calls.length, 1, 'a cached row never asks again');
  assert.equal(loader.peek('c', 2, 2), undefined, 'a new revision is a new question');
});

test('a failed request settles its waiters and caches nothing, so the row can ask again', async () => {
  let fail = true;
  const loader = new TurnLoader(async (_c, ids) => {
    if (fail) throw new Error('boom');
    return ids.map(card);
  }, 1);
  await loader.load('c', 7, 1);
  assert.equal(loader.peek('c', 7, 1), undefined);
  fail = false;
  await loader.load('c', 7, 1);
  assert.deepEqual(loader.peek('c', 7, 1), card(7));
});
