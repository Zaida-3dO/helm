import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import type { ChatOps } from '../api/ops.ts';
import { canForkAt, forkAction } from './forkAction.ts';

test('only complete assistant replies are fork points, and a fork opens the new chat', async () => {
  assert.equal(canForkAt(row({ role: 'assistant' })), true);
  assert.equal(canForkAt(row({ role: 'assistant', status: 'streaming' })), false);
  assert.equal(canForkAt(row({ role: 'user' })), false);
  const calls: unknown[] = [];
  const ops = { fork: async (chatId: string, opts: unknown) => (calls.push([chatId, opts]), { id: 'f1' }) } as unknown as ChatOps;
  let opened = '';
  const a = forkAction(ops, (id) => (opened = id), () => undefined);
  a.run(row({ role: 'assistant', id: 7 }), { chatId: 'c1', isLatestAssistant: true, running: false, interactionOpen: false });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls, [['c1', { messageId: 7 }]]);
  assert.equal(opened, 'f1');
});
