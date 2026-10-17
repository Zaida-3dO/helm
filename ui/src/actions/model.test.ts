import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ActionButton } from '../api/actions.ts';
import {
  addButton,
  firstProblem,
  MAX_BUTTONS,
  moveButton,
  newButton,
  overflowButtons,
  problemWith,
  removeButton,
  topbarButtons,
  updateButton,
} from './model.ts';

const b = (id: string, over: Partial<ActionButton> = {}): ActionButton => ({
  ...newButton(id),
  label: id.toUpperCase(),
  text: `send ${id}`,
  ...over,
});

test('the top bar shows visible top-bar buttons in order; the menu shows visible overflow ones', () => {
  const set = {
    buttons: [b('a'), b('h', { hidden: true }), b('o', { placement: 'overflow' }), b('c'), b('oh', { placement: 'overflow', hidden: true })],
  };
  assert.deepEqual(topbarButtons(set).map((x) => x.id), ['a', 'c']);
  assert.deepEqual(overflowButtons(set).map((x) => x.id), ['o']);
});

test('limits: label 1-40 characters, text 1 byte to 16 KiB, prompt buttons have no project', () => {
  assert.equal(problemWith(b('a')), null);
  assert.match(problemWith(b('a', { label: '  ' }))!, /label/);
  assert.equal(problemWith(b('a', { label: 'x'.repeat(40) })), null);
  assert.match(problemWith(b('a', { label: 'x'.repeat(41) }))!, /40/);
  assert.match(problemWith(b('a', { text: '' }))!, /text/);
  assert.equal(problemWith(b('a', { text: 'x'.repeat(16 * 1024) })), null);
  assert.match(problemWith(b('a', { text: 'x'.repeat(16 * 1024 + 1) }))!, /16 KiB/);
  assert.match(problemWith(b('a', { projectId: 'p' }))!, /new-chat/);
  assert.equal(problemWith(b('a', { kind: 'new_chat', projectId: 'p' })), null);
});

test('text size counts bytes, not characters', () => {
  assert.match(problemWith(b('a', { text: 'é'.repeat(9000) }))!, /16 KiB/);
});

test('firstProblem names the offending button and caps the count', () => {
  assert.equal(firstProblem([b('a'), b('c')]), null);
  assert.deepEqual(firstProblem([b('a'), b('bad', { label: '' })])?.id, 'bad');
  const many = Array.from({ length: MAX_BUTTONS + 1 }, (_, i) => b(`b${i}`));
  assert.match(firstProblem(many)!.message, /24/);
});

test('add stops at the cap', () => {
  const full = Array.from({ length: MAX_BUTTONS }, (_, i) => b(`b${i}`));
  assert.equal(addButton(full, b('x')).length, MAX_BUTTONS);
  assert.equal(addButton([b('a')], b('x')).length, 2);
});

test('update keeps the id and the seed link, and unbinds a prompt button', () => {
  const seeded = b('s', { seedId: 'update', kind: 'new_chat', projectId: 'p' });
  const [u] = updateButton([seeded], 's', { id: 'other', seedId: null, label: 'New' });
  assert.equal(u!.id, 's');
  assert.equal(u!.seedId, 'update');
  assert.equal(u!.label, 'New');
  const [p] = updateButton([seeded], 's', { kind: 'prompt' });
  assert.equal(p!.projectId, null);
});

test('hide is an update, not a removal', () => {
  const [h] = updateButton([b('a')], 'a', { hidden: true });
  assert.equal(h!.hidden, true);
  assert.deepEqual(removeButton([b('a'), b('c')], 'a').map((x) => x.id), ['c']);
});

test('move swaps with a neighbour and ignores the ends', () => {
  const list = [b('a'), b('c'), b('d')];
  assert.deepEqual(moveButton(list, 'c', -1).map((x) => x.id), ['c', 'a', 'd']);
  assert.deepEqual(moveButton(list, 'c', 1).map((x) => x.id), ['a', 'd', 'c']);
  assert.equal(moveButton(list, 'a', -1), list);
  assert.equal(moveButton(list, 'd', 1), list);
  assert.equal(moveButton(list, 'zzz', 1), list);
});
