import { test } from 'node:test';
import assert from 'node:assert/strict';
import { countLabel, findMatches, step } from './find.ts';
import { row } from '../dev/fixtures.ts';

const rows = [
  row({ id: 1, role: 'user', text: 'Fix the Sidebar, then the sidebar menu' }),
  row({ id: 2, role: 'assistant', text: 'Looking', thinking: 'the sidebar is in Sidebar.tsx' }),
  row({ id: 3, role: 'toolResult', toolName: 'Read', details: { toolArgs: '{"file_path":"src/sidebar/Sidebar.tsx"}' } }),
  row({ id: 4, role: 'assistant', text: 'Done: the sidebar menu now opens.' }),
];

test('matches are case-insensitive, oldest first, one entry per occurrence', () => {
  const { matches } = findMatches(rows, 'sidebar', false);
  assert.deepEqual(matches, [
    { rowId: 1, occurrence: 0 },
    { rowId: 1, occurrence: 1 },
    { rowId: 4, occurrence: 0 },
  ]);
});

test('"Include activity" adds thinking and tool calls', () => {
  const ids = findMatches(rows, 'sidebar', true).matches.map((m) => m.rowId);
  assert.deepEqual(ids, [1, 1, 2, 2, 3, 3, 4]);
});

test('an empty query matches nothing, and the list stops at the cap', () => {
  assert.equal(findMatches(rows, '  ', true).matches.length, 0);
  const capped = findMatches(rows, 'sidebar', true, 3);
  assert.equal(capped.matches.length, 3);
  assert.equal(capped.capped, true);
});

test('stepping starts at the newest match and wraps both ways', () => {
  assert.equal(step(-1, 5, 'older'), 4);
  assert.equal(step(4, 5, 'older'), 3);
  assert.equal(step(0, 5, 'older'), 4);
  assert.equal(step(4, 5, 'newer'), 0);
  assert.equal(step(2, 0, 'newer'), -1);
  assert.equal(countLabel(2, 120, false), '3 / 120');
  assert.equal(countLabel(0, 10_000, true), '1 / 10000+');
  assert.equal(countLabel(-1, 0, false), 'No matches');
});
