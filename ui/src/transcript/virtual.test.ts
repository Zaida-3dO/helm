import { test } from 'node:test';
import assert from 'node:assert/strict';
import { anchorAt, ESTIMATE, estimateHeight, Geometry, HeightCache, restoreAnchor, restoreAnchors } from './virtual.ts';

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

test('restoreAnchors falls back to the next anchor when the first leaves the list', () => {
  const g = new Geometry();
  for (const k of ['a', 'b', 'c', 'd']) g.measure(k, 100);
  const anchors = [
    { key: 'b', delta: -20 },
    { key: 'c', delta: 80 },
  ];
  // 'b' folded away; 'c' now sits right after 'a'.
  const keys = ['a', 'c', 'd'];
  assert.equal(restoreAnchors(keys, g.offsets(keys), anchors), 100 - 80);
  assert.equal(restoreAnchors(['x'], [0, 10], anchors), null);
  assert.equal(restoreAnchors(keys, g.offsets(keys), []), null);
});

test('a cached height from another visit is used at the same width only, and a fresh measure wins', () => {
  const cache = new HeightCache();
  const first = new Geometry(cache);
  first.width = 800;
  first.measure('r:1', 300);
  // A later visit (a new list after a chat switch) starts from the cached height.
  const again = new Geometry(cache);
  again.estimate = () => 50;
  again.width = 800;
  assert.equal(again.height('r:1'), 300);
  assert.equal(again.height('r:2'), 50, 'never measured: the estimate');
  again.width = 900;
  assert.equal(again.height('r:1'), 50, 'another width: the cached height does not apply');
  again.measure('r:1', 280);
  assert.equal(again.height('r:1'), 280);
  // Nothing is cached before the width is known.
  const blind = new Geometry(cache);
  blind.measure('r:3', 10);
  blind.width = 800;
  assert.equal(new Geometry(cache).height('r:3'), ESTIMATE);
});

test('the height cache is bounded, least recently set first out', () => {
  const cache = new HeightCache(2);
  cache.set('a', 1, 10);
  cache.set('b', 1, 20);
  cache.set('a', 1, 11);
  cache.set('c', 1, 30);
  assert.equal(cache.size, 2);
  assert.equal(cache.get('b', 1), undefined);
  assert.equal(cache.get('a', 1), 11);
});

test('estimates grow with text and differ by kind', () => {
  const short = estimateHeight({ kind: 'assistant', chars: 40, lines: 0 });
  const long = estimateHeight({ kind: 'assistant', chars: 4000, lines: 40 });
  assert.ok(long > short * 10, `${long} vs ${short}`);
  assert.ok(estimateHeight({ kind: 'user', chars: 20, lines: 0 }) > estimateHeight({ kind: 'fold', chars: 0, lines: 0 }));
  assert.ok(estimateHeight({ kind: 'user', chars: 2000, lines: 0 }) > estimateHeight({ kind: 'user', chars: 20, lines: 0 }));
  assert.equal(estimateHeight({ kind: 'toolResult', chars: 99_999, lines: 999 }), estimateHeight({ kind: 'toolResult', chars: 0, lines: 0 }), 'tool cards start collapsed');
});
