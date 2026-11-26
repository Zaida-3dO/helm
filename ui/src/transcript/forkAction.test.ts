import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import type { ChatOps } from '../api/ops.ts';
import { canForkAt, forkAction } from './forkAction.ts';
import { anchorSession, isEarlierSession } from './session.ts';

test('only complete assistant replies are fork points, and a fork opens the new chat', async () => {
  assert.equal(canForkAt(row({ role: 'assistant' })), true);
  assert.equal(canForkAt(row({ role: 'assistant', status: 'streaming' })), false);
  assert.equal(canForkAt(row({ role: 'user' })), false);
  const calls: unknown[] = [];
  const ops = { fork: async (chatId: string, opts: unknown) => (calls.push([chatId, opts]), { id: 'f1' }) } as unknown as ChatOps;
  let opened = '';
  const a = forkAction(ops, (id) => (opened = id), () => undefined);
  a.run(row({ role: 'assistant', id: 7 }), { chatId: 'c1', sessionId: 'c1', isLatestAssistant: true, running: false, interactionOpen: false });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls, [['c1', { messageId: 7 }]]);
  assert.equal(opened, 'f1');
  // A reply of an earlier session of a rolled-over chat forks from the session that holds it, not
  // from the window's chat: the host looks the message up under its own session.
  a.run(row({ role: 'assistant', id: 3, chatId: 'old' }), { chatId: 'c1', sessionId: 'old', isLatestAssistant: false, running: false, interactionOpen: false });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls.at(-1), ['old', { messageId: 3 }]);
  // Fork is offered on a chat that runs on another PC as well: the action names the chat, and
  // the host routes the fork to the PC that runs it.
  assert.equal(a.localOnly, undefined);
});

test('rows of an earlier session are told apart, and a selection anchors in the session that holds it', () => {
  assert.equal(isEarlierSession({ chatId: 'new' }, 'new'), false);
  assert.equal(isEarlierSession({ chatId: 'old' }, 'new'), true);
  const rows = [{ id: 1, chatId: 'old' }, { id: 2, chatId: 'new' }];
  assert.equal(anchorSession(rows, 1, 'new'), 'old');
  assert.equal(anchorSession(rows, 2, 'new'), 'new');
  assert.equal(anchorSession(rows, 9, 'new'), 'new', 'an anchor that is not loaded falls back to the window chat');
});
