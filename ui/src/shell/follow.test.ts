import { test } from 'node:test';
import assert from 'node:assert/strict';
import { carryDraft, carryOnRollOver, decideOpen, followRollOver, mergedDraft, openTarget } from './follow.ts';

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

// The window following a roll over: the panel carry and the draft carry are one step, and both
// happen only for a real roll over, and after the composers have saved what was typed.
test('following a roll over carries the panel, saves the composers, then carries typed text', async () => {
  const chats: Record<string, { prevSessionId: string | null; draft: string; draftRevision: number }> = {
    c1: { prevSessionId: null, draft: 'typed after the move', draftRevision: 50 },
    c2: { prevSessionId: 'c1', draft: '', draftRevision: 40 },
  };
  const log: string[] = [];
  const deps = {
    loadChat: async (id: string) => chats[id]!,
    carryPanel: (from: string, to: string) => void log.push(`panel ${from}>${to}`),
    flush: async () => void log.push('flush'),
    saveDraft: async (id: string, text: string, rev: number) => void log.push(`save ${id} "${text}" ${rev > 50 ? 'newer' : 'older'}`),
  };
  assert.equal(await followRollOver({ chatId: 'c2', ifViewing: 'c1' }, deps), true);
  assert.deepEqual(log, ['panel c1>c2', 'flush', 'save c2 "typed after the move" newer', 'save c1 "" newer']);
  // Not a roll over: nothing moves.
  log.length = 0;
  assert.equal(await followRollOver({ chatId: 'c1', ifViewing: 'c2' }, deps), false);
  assert.deepEqual(log, []);
});

test('typed text carries to the successor without overwriting what it holds', async () => {
  // [old draft, successor draft, what is saved to the successor (null: nothing is)]
  const cases: [string, string, string | null][] = [
    ['typed', '', 'typed'], // an empty successor takes it
    ['carried and more', 'carried', 'carried and more'], // the later part of the same text
    ['carried', 'carried and more', null], // the successor already holds more
    ['typed', 'something else', 'something else\n\ntyped'], // different text: both kept, nothing overwritten
    ['  ', 'kept', null], // nothing typed in the old one
  ];
  for (const [oldDraft, successor, want] of cases) {
    const saved: [string, string, number][] = [];
    const draft = (d: string, draftRevision: number) => async () => ({ draft: d, draftRevision });
    const holders: Record<string, () => Promise<{ draft: string; draftRevision: number }>> = { old: draft(oldDraft, 10), new: draft(successor, 20) };
    const done = await carryDraft('old', 'new', (id) => holders[id]!(), async (id, text, rev) => void saved.push([id, text, rev]), () => 5);
    const toNew = saved.find((s) => s[0] === 'new');
    assert.equal(toNew?.[1] ?? null, want, JSON.stringify([oldDraft, successor]));
    if (toNew) assert.equal(toNew[2], 21, 'newer than what the successor holds');
    assert.equal(done, oldDraft.trim() !== '');
    assert.deepEqual(saved.filter((s) => s[0] === 'old'), oldDraft.trim() !== '' ? [['old', '', 11]] : []);
  }
  assert.equal(mergedDraft('a b', ''), 'a b');
  // A failing read or save carries nothing and says so.
  assert.equal(await carryDraft('old', 'new', async () => Promise.reject(new Error('gone')), async () => undefined), false);
});
