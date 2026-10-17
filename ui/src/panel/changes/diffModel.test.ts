import assert from 'node:assert/strict';
import { test } from 'node:test';
import { counts, fold, parseDiff } from './diffModel.ts';
import type { DiffRow, Gap } from './diffModel.ts';

const patch = [
  'diff --git a/a.txt b/a.txt',
  'index 1..2 100644',
  '--- a/a.txt',
  '+++ b/a.txt',
  '@@ -1,12 +1,12 @@',
  ...Array.from({ length: 10 }, (_, i) => ` line ${i + 1}`),
  '-old 11',
  '+new 11',
  ' line 12',
  '\\ No newline at end of file',
].join('\n');

test('parseDiff numbers both gutters and drops file headers', () => {
  const rows = parseDiff(patch);
  assert.equal(rows[0]!.kind, 'hunk');
  const del = rows.find((r) => r.kind === 'del')!;
  const add = rows.find((r) => r.kind === 'add')!;
  assert.deepEqual([del.oldNo, del.text], [11, 'old 11']);
  assert.deepEqual([add.newNo, add.text], [11, 'new 11']);
  const last = rows.filter((r) => r.kind === 'ctx').at(-1)!;
  assert.deepEqual([last.oldNo, last.newNo], [12, 12]);
  assert.equal(rows.at(-1)!.kind, 'meta');
  assert.deepEqual(counts(rows), { added: 1, removed: 1 });
  assert.ok(!rows.some((r) => r.text.startsWith('diff --git') || r.text.startsWith('++')));
});

test('fold hides long unchanged runs but keeps three lines next to a change', () => {
  const shown = fold(parseDiff(patch));
  const gap = shown.find((r) => r.kind === 'gap') as Gap;
  assert.equal(gap.rows.length, 7, 'lines 1-7 hidden; 8-10 kept above the change');
  const visibleCtx = shown.filter((r): r is DiffRow => r.kind === 'ctx').map((r) => r.newNo);
  assert.deepEqual(visibleCtx, [8, 9, 10, 12]);
});

test('expanding a gap reveals lines from its top, and fully expanded it disappears', () => {
  const rows = parseDiff(patch);
  const some = fold(rows, new Map([[0, 5]]));
  assert.equal((some.find((r) => r.kind === 'gap') as Gap).rows.length, 2);
  const all = fold(rows, new Map([[0, 99]]));
  assert.ok(!all.some((r) => r.kind === 'gap'));
  assert.equal(all.filter((r) => r.kind === 'ctx').length, 11);
});

test('short runs are never folded', () => {
  const p = '@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n';
  assert.ok(!fold(parseDiff(p)).some((r) => r.kind === 'gap'));
});
