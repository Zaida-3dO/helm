import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatElapsed, layout, type Item } from './workBlocks.ts';
import { row } from '../dev/fixtures.ts';

function turn() {
  return [
    row({ id: 1, role: 'user', text: 'do it', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'let me look', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'toolResult', toolName: 'Bash', status: 'error', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'done', timestamp: 65_000 }),
  ];
}

const shape = (items: Item[]) =>
  items.map((i) => (i.kind === 'row' ? i.row.id : `fold[${i.rows.map((r) => r.id).join(',')}]`));

test('a settled turn folds its work, keeping errors and the final reply', () => {
  const items = layout(turn(), false);
  assert.deepEqual(shape(items), [1, 'fold[2,3]', 4, 5]);
  const fold = items.find((i) => i.kind === 'fold');
  // Timed from the run's first output (1 s), not from the prompt (0 s).
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 64_000);
});

test('a turn with no tool call and no thinking does not fold, however long it took to start', () => {
  const rows = [
    row({ id: 1, role: 'user', text: 'Hello', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'Hi! What are we building?', timestamp: 31_000 }),
  ];
  assert.deepEqual(shape(layout(rows, false)), [1, 2, 3]);
});

test('thinking alone is work: the turn folds, timed from its first output', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', thinking: 'weighing it up', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'answer', timestamp: 42_000 }),
  ];
  const items = layout(rows, false);
  assert.deepEqual(shape(items), [1, 'fold[2]', 3]);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 12_000);
});

test('the row right after a closed fold is marked, so it draws no second rule', () => {
  const items = layout(turn(), false);
  const flag = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.afterFold;
  };
  assert.equal(flag(4), true);
  assert.equal(flag(5), false);
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'toolResult' }), row({ id: 3, role: 'assistant', text: 'done' })], false);
  const done = plain.find((i) => i.kind === 'row' && i.row.id === 3);
  assert.equal(done?.kind === 'row' && done.final && done.afterFold, true);
  // Opened, the work sits between the fold and the reply, so the reply keeps its hairline.
  const open = layout(turn(), false, new Set(['k1']));
  const reply = open.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.afterFold, false);
});

test('a live turn never folds', () => {
  assert.deepEqual(shape(layout(turn(), true)), [1, 2, 3, 4, 5]);
});

test('an earlier turn folds while the latest is live; expanding unfolds it', () => {
  const rows = [
    ...turn(),
    row({ id: 6, role: 'user', timestamp: 70_000 }),
    row({ id: 7, role: 'toolResult', timestamp: 71_000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[2,3]', 4, 5, 6, 7]);
  // Opened: the "Worked for" row stays (so it can be closed again) and the work follows in order.
  assert.deepEqual(shape(layout(rows, true, new Set(['k1']))), [1, 'fold[2,3]', 2, 3, 4, 5, 6, 7]);
});

test('context, retry and interrupted rows stay visible', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'context', text: 'compacted' }),
    row({ id: 3, role: 'assistant', text: 'x', details: { retry: { state: 'recovered', attempt: 1 } } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 8, role: 'toolResult', status: 'interrupted' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[4]', 2, 3, 8, 5]);
});

test('formatElapsed', () => {
  assert.equal(formatElapsed(4_400), '4s');
  assert.equal(formatElapsed(65_000), '1m 5s');
  assert.equal(formatElapsed(3_720_000), '1h 2m');
});

test('a steer read mid-turn stays in the turn and is never folded', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult' }),
    row({ id: 3, role: 'user', text: 'also this', details: { mode: 'steer' } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 2, 3, 4, 5]);
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[2,4]', 3, 5]);
});

test('the fold counts failed tool calls and flags the final answer', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 3, role: 'toolResult', toolName: 'Read' }),
    row({ id: 4, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 5, role: 'assistant', text: 'done' }),
  ];
  const items = layout(rows, false);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.failed, 2);
  const reply = items.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.final, true);
  assert.equal(reply?.kind === 'row' && reply.afterTool, true);
  // A plain answer with no tool work has no hairline.
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'assistant', text: 'hi' })], false);
  const hi = plain.find((i) => i.kind === 'row' && i.row.id === 2);
  assert.equal(hi?.kind === 'row' && hi.final, false);
});

test('opened work is indented, the shown rows are not', () => {
  const items = layout(turn(), false, new Set(['k1']));
  const ind = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.indent;
  };
  assert.equal(ind(2), true);
  assert.equal(ind(5), false);
});
