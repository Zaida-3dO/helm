import assert from 'node:assert/strict';
import { test } from 'node:test';
import { prefersReducedMotion, shouldPace } from './motion.ts';

test('Reduce Motion shows streaming text at once', () => {
  assert.equal(shouldPace(true, false, true), false);
  assert.equal(shouldPace(false, true, true), false);
  assert.equal(shouldPace(true, false, false), true);
  assert.equal(shouldPace(false, true, false), true, 'a pacer already running finishes its reveal');
  assert.equal(shouldPace(false, false, false), false);
});

test('the media query is read, and missing matchMedia means motion is allowed', () => {
  const g = globalThis as { matchMedia?: (q: string) => { matches: boolean } };
  const saved = g.matchMedia;
  try {
    delete g.matchMedia;
    assert.equal(prefersReducedMotion(), false);
    let asked = '';
    g.matchMedia = (q) => {
      asked = q;
      return { matches: true };
    };
    assert.equal(prefersReducedMotion(), true);
    assert.equal(asked, '(prefers-reduced-motion: reduce)');
  } finally {
    if (saved) g.matchMedia = saved;
    else delete g.matchMedia;
  }
});
