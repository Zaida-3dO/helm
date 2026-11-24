// The chat's open-item state patches (cards opened, row expanded) and the merge the sidebar
// draws with. These mirror the host's `chat_set_sidebar_state` rules.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { SidebarRow } from '../api/projects.ts';
import type { SidebarState } from '../api/types.ts';
import { effectiveExpanded, pruneToggled } from './model.ts';
import { MAX_OPEN_CARDS, cardOpen, patchCardOpen, patchExpanded } from './state.ts';

test('the patches carry one field each, so the two never erase each other', () => {
  assert.deepEqual(patchExpanded(true), { v: 1, expanded: true });
  assert.deepEqual(patchExpanded(false), { v: 1, expanded: false });
  const opened: SidebarState = patchCardOpen(null, 7, true);
  assert.deepEqual(opened, { v: 1, openCards: [7] });
  assert.ok(!("expanded" in opened), "opening a card says nothing about the row");
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
  assert.ok(s, "the loop opened cards");
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

test('local toggle bridges ride on until the chat record agrees, then are dropped', () => {
  const rows = [row('a', true), row('b', false), row('c'), row('d', true)];
  // An expand whose record now says open, and a fold whose record now says folded: the
  // bridges are dead weight, dropping them lets a later change from another PC through.
  let p = pruneToggled(rows, new Set(['a', 'c']), new Set(['b', 'd']), new Set());
  assert.deepEqual([...p.ephemeral], ['c'], 'a: the record caught up; c: still a pending expand');
  assert.deepEqual([...p.shut], ['d'], 'b: the shut write answered; d: record still open');
  assert.ok(p.changed);
  // Rows this window does not hold say nothing either way; both bridges ride on.
  p = pruneToggled(rows, new Set(['z']), new Set(['y']), new Set());
  assert.deepEqual([...p.ephemeral], ['z']);
  assert.deepEqual([...p.shut], ['y']);
  assert.ok(!p.changed, 'nothing dropped');
  // The open chat's ancestors: a shut one stays shut while that chat is open — the person's
  // collapse here outranks the auto-expansion aid.
  p = pruneToggled(rows, new Set(['c']), new Set(['b']), new Set(['b', 'c']));
  assert.deepEqual([...p.shut], ['b'], 'the collapsed ancestor keeps its shut');
  assert.deepEqual([...p.ephemeral], ['c']);
  assert.ok(!p.changed);
  // Idempotent: pruning again changes nothing.
  p = pruneToggled(rows, p.ephemeral, p.shut, new Set());
  assert.ok(!p.changed);
});
