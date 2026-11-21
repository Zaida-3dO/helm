import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decideOpen } from './follow.ts';

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
