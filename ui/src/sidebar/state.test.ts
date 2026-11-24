// The chat's open-item state patches (cards opened, row expanded) and the merge the sidebar
// draws with. These mirror the host's `chat_set_sidebar_state` rules.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { SidebarRow } from '../api/projects.ts';
import type { SidebarState } from '../api/types.ts';
import { effectiveExpanded } from './model.ts';
import { MAX_OPEN_CARDS, cardOpen, patchCardOpen, patchExpanded } from './state.ts';

test('the patches carry one field each, so the two never erase each other', () => {
  assert.deepEqual(patchExpanded(true), { v: 1, expanded: true });
  assert.deepEqual(patchExpanded(false), { v: 1, expanded: false });
  const opened = patchCardOpen(null, 7, true);
  assert.deepEqual(opened, { v: 1, openCards: [7] });
  assert.equal(opened.expanded, undefined, "opening a card says nothing about the row");
  assert.equal(patchExpanded(true).openCards, undefined, "expanding says nothing about the cards");
});

test('card patches add, move and fold by message id', () => {
  let s: SidebarState | null = patchCardOpen(null, 7, true);
  assert(cardOpen(s, 7));
  assert(!cardOpen(s, 9));
  s = patchCardOpen(s, 9, true);
  assert.deepEqual(s.openCards, [7, 9]);
  // Re-opening a card that is already open leaves one entry, moved to the back.
  s = patchCardOpen(s, 7, true);
  assert.deepEqual(s.openCards, [9, 7]);
  // Folding one card leaves the others.
  s = patchCardOpen(s, 9, false);
  assert.deepEqual(s.openCards, [7]);
  // Folding an already-folded card changes nothing.
  s = patchCardOpen(s, 3, false);
  assert.deepEqual(s.openCards, [7]);
  // An empty list is a real sentence: everything folded.
  s = patchCardOpen(s, 7, false);
  assert.deepEqual(s.openCards, []);
  assert(!cardOpen(s, 7));
  // A missing previous state means an all-folded start.
  assert.deepEqual(patchCardOpen(undefined, 7, true).openCards, [7]);
});

test('the opened-card list stays under its cap, dropping the oldest first', () => {
  let s: SidebarState | null = null;
  for (let i = 1; i <= MAX_OPEN_CARDS + 5; i++) s = patchCardOpen(s, i, true);
  assert.equal(s.openCards!.length, MAX_OPEN_CARDS);
  assert.equal(s.openCards![0], 6, "the five oldest opened were dropped");
  assert(cardOpen(s, MAX_OPEN_CARDS + 5));
});

const row = (chatId: string, expanded?: boolean): SidebarRow =>
  ({ chatId, expanded }) as unknown as SidebarRow;

test('the drawn expansion is the chat state plus this window aids, minus what was shut here', () => {
  const rows = [row('a', true), row('b', false), row('c'), row('d', true)];
  // The chats whose own record says open; nothing shut, no window aids.
  assert.deepEqual([...effectiveExpanded(rows, new Set(), new Set())], ['a', 'd']);
  // A window auto-expansion adds one; a chat shut in this window drops, whether its record
  // says open or the window just added it (until the shut write answers).
  const shut = new Set(['a', 'e']);
  assert.deepEqual([...effectiveExpanded(rows, new Set(['e']), shut)], ['d']);
  // A chat whose record never opened still shows while the window holds it.
  assert.deepEqual([...effectiveExpanded(rows, new Set(['c']), new Set())].sort(), ['a', 'c', 'd']);
});
