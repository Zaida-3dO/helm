import { test } from 'node:test';
import assert from 'node:assert/strict';
import { anchorAt, ESTIMATE, Geometry, restoreAnchor } from './virtual.ts';

test('offsets use measured heights and the estimate for the rest', () => {
  const g = new Geometry();
  g.measure('a', 10);
  assert.equal(g.measure('a', 10), false, 'an unchanged height is not a change');
  assert.deepEqual(g.offsets(['a', 'b']), [0, 10, 10 + ESTIMATE]);
});

test('only rows near the viewport are in the window', () => {
  const g = new Geometry();
  const keys = Array.from({ length: 1000 }, (_, i) => `k${i}`);
  for (const k of keys) g.measure(k, 100);
  const o = g.offsets(keys);
  const w = g.window(o, 50_000, 800, 200);
  assert.deepEqual(w, { start: 498, end: 510 });
});

test('prepending older rows keeps the anchor row in place', () => {
  const g = new Geometry();
  const keys = ['c', 'd', 'e'];
  for (const k of keys) g.measure(k, 100);
  const before = g.offsets(keys);
  const anchor = anchorAt(keys, before, 130);
  assert.deepEqual(anchor, { key: 'd', delta: -30 });
  const more = ['a', 'b', ...keys];
  g.measure('a', 50);
  g.measure('b', 70);
  const after = g.offsets(more);
  assert.equal(restoreAnchor(more, after, anchor!), 50 + 70 + 100 + 30);
});
