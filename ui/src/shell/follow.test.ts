import { test } from 'node:test';
import assert from 'node:assert/strict';
import { carryOnRollOver, decideOpen, openTarget } from './follow.ts';

// (event, chat on screen, expected decision): the window follows when the host names no guard
// or the user is still looking at the guarded chat; otherwise the user only gets a notice.
test('an open-chat event follows, notices or is ignored by what it carries', () => {
  const cases: [Record<string, unknown>, string | null, ReturnType<typeof decideOpen>][] = [
    // The tray or the pet: no guard, always select; a messageId rides along.
    [{ chatId: 'c2', messageId: 42 }, 'c1', { kind: 'select', chatId: 'c2', messageId: '42' }],
    // The chat that started it is still on screen: follow.
    [{ chatId: 'c2', ifViewing: 'c1', title: 'Next' }, 'c1', { kind: 'select', chatId: 'c2', messageId: undefined }],
    // The user moved on: a notice naming the new chat instead of pulling the window.
    [{ chatId: 'c2', ifViewing: 'c1', title: 'Next' }, 'c9', { kind: 'notice', chatId: 'c2', title: 'Next' }],
    // A notice without a usable title falls back in the UI, not here.
    [{ chatId: 'c2', ifViewing: 'c1', title: '  ' }, null, { kind: 'notice', chatId: 'c2', title: null }],
  ];
  for (const [event, current, want] of cases) {
    assert.deepEqual(decideOpen(event, current), want, JSON.stringify(event));
  }
  assert.equal(decideOpen({ ifViewing: 'c1' }, 'c1'), null, 'an event with no chat id does nothing');
});

// A rolled-over session is not where the chat lives: the window to open is the stack's newest
// session (one window spans the whole stack); a chat that never rolled over opens as itself.
test('a rolled-over session id opens the newest session of its stack', () => {
  const cases: [string, string | null | undefined, string][] = [
    ['c1', 'c3', 'c3'], // oldest of three: straight to the newest
    ['c2', 'c3', 'c3'], // the middle session: on to the newest
    ['c3', 'c3', 'c3'], // already the newest
    ['c1', null, 'c1'], // no stack
    ['c1', undefined, 'c1'],
    ['c1', '', 'c1'],
  ];
  for (const [chatId, latest, want] of cases) assert.equal(openTarget(chatId, latest), want, `${chatId} -> ${String(latest)}`);
});

test('the side panel goes with a roll over, and only with one', async () => {
  const moved: string[][] = [];
  const carry = (from: string, to: string) => void moved.push([from, to]);
  const successor = async () => ({ prevSessionId: 'c1' });
  assert.equal(await carryOnRollOver({ chatId: 'c2', ifViewing: 'c1' }, successor, carry), true);
  assert.deepEqual(moved, [['c1', 'c2']]);
  // A chat an agent started names its own chat too, but it did not come out of a roll over.
  assert.equal(await carryOnRollOver({ chatId: 'c3', ifViewing: 'c1' }, async () => ({ prevSessionId: null }), carry), false);
  // Nothing to carry from, nothing to carry to, or a chat that cannot be read.
  assert.equal(await carryOnRollOver({ chatId: 'c2' }, successor, carry), false);
  assert.equal(await carryOnRollOver({ ifViewing: 'c1' }, successor, carry), false);
  assert.equal(await carryOnRollOver({ chatId: 'c2', ifViewing: 'c1' }, async () => Promise.reject(new Error('gone')), carry), false);
  assert.equal(moved.length, 1);
});
