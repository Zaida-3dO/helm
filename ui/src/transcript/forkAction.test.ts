import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import type { ChatOps } from '../api/ops.ts';
import { canForkAt, forkAction } from './forkAction.ts';
import { canEditRow, anchorSession, isEarlierSession, slotContextFor } from './session.ts';
import { savedFromRow } from '../saved/model.ts';
import { goBackTarget } from '../checkpoints/model.ts';
import { turnCardTarget } from '../panel/turns/loader.ts';
import { recapPictureChat } from './recap.ts';

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

// A window over a rolled-over chat holds rows of earlier sessions. Every action on such a row
// addresses the session that wrote it: the context built for the row names it, and each action
// reads it from there. Replacing the row's own session by the window's chat breaks every line.
test('message actions on a row of an earlier session address that session, not the window chat', async () => {
  const inputs = { chatId: 'new', latestAssistantId: null, running: false, interactionOpen: false };
  const forked: string[] = [];
  const ops = { fork: async (chatId: string) => (forked.push(chatId), { id: 'f1' }) } as unknown as ChatOps;
  const fork = forkAction(ops, () => undefined, () => undefined);
  for (const [chatId, want] of [['old', 'old'], ['new', 'new']] as const) {
    const reply = row({ role: 'assistant', id: 4, chatId, revision: 3, text: 'Done.' });
    const ctx = slotContextFor(reply, inputs);
    assert.equal(ctx.chatId, 'new', 'a message an action sends still goes to the window chat');
    assert.equal(ctx.sessionId, want);
    fork.run(reply, ctx);
    await new Promise((r) => setTimeout(r, 0));
    const addressed: [string, string][] = [
      ['fork', forked.at(-1)!],
      ['save for later', savedFromRow(reply, ctx).chatId],
      ['go back', goBackTarget(reply, ctx).chatId],
      ['turn card', turnCardTarget(reply, ctx).chatId],
      ['recap pictures', recapPictureChat(ctx)],
    ];
    for (const [action, got] of addressed) assert.equal(got, want, `${action} on a row of ${chatId}`);
    assert.deepEqual(savedFromRow(reply, ctx), { chatId: want, messageId: 4, title: 'Done.', body: 'Done.' });
    assert.deepEqual(turnCardTarget(reply, ctx), { chatId: want, messageId: 4, revision: 3 });
  }
});

// The pencil (edit and resend) is offered only on a finished message of the window's own session:
// the host refuses to rewrite an archived session.
test('the pencil is offered on the window session only, and only when nothing else forbids it', () => {
  const win = { chatId: 'new', running: false };
  const open = { editor: true, note: false, page: false };
  const cases: [string, Parameters<typeof canEditRow>, boolean][] = [
    ['own finished message', [{ status: 'complete', chatId: 'new' }, win, open], true],
    ['earlier session', [{ status: 'complete', chatId: 'old' }, win, open], false],
    ['turn running', [{ status: 'complete', chatId: 'new' }, { ...win, running: true }, open], false],
    ['unfinished', [{ status: 'streaming', chatId: 'new' }, win, open], false],
    ['a note', [{ status: 'complete', chatId: 'new' }, win, { ...open, note: true }], false],
    ['a page message', [{ status: 'complete', chatId: 'new' }, win, { ...open, page: true }], false],
    ['no editor', [{ status: 'complete', chatId: 'new' }, win, { ...open, editor: false }], false],
  ];
  for (const [name, args, want] of cases) assert.equal(canEditRow(...args), want, name);
});
