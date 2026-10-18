import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inertLines } from './inert.ts';
import { SETTLE_MS, SettleGate } from './settle.ts';

const r = (y = 10) => ({ x: 10, y, width: 80, height: 28 });

test('a click before 400 ms of stability decides nothing; after it, it counts', () => {
  const g = new SettleGate(0);
  g.frame(0, new Map([['allow', r()]]));
  assert.equal(g.canDecide('allow', 100, r()), false, '100 ms after mount');
  g.frame(200, new Map([['allow', r()]]));
  assert.equal(g.canDecide('allow', SETTLE_MS - 1, r()), false);
  assert.equal(g.canDecide('allow', SETTLE_MS, r()), true);
});

test('movement, a missing reference, or a reset restarts the window', () => {
  const g = new SettleGate(0);
  g.frame(0, new Map([['allow', r()]]));
  g.frame(300, new Map([['allow', r(10.4)]]));
  assert.equal(g.canDecide('allow', 450, r(10.4)), true, 'within 0.5 px is not movement');
  g.frame(500, new Map([['allow', r(12)]]));
  assert.equal(g.canDecide('allow', 800, r(12)), false, 'moved at 500');
  assert.equal(g.canDecide('allow', 900, r(12)), true);
  assert.equal(g.canDecide('allow', 950, r(20)), false, 'moved between the frame and the click');
  assert.equal(g.canDecide('deny', 2000, r()), false, 'no reference yet');
  g.reset(3000);
  assert.equal(g.canDecide('allow', 3300, r(20)), false);
  g.frame(3300, new Map([['allow', r(20)]]));
  assert.equal(g.canDecide('allow', 3700, r(20)), true);
});

test('a card leaving restarts the window for the others', () => {
  const g = new SettleGate(0);
  g.frame(0, new Map([['a', r()], ['b', r(50)]]));
  g.frame(500, new Map([['b', r(50)]]));
  assert.equal(g.canDecide('b', 600, r(50)), false);
  assert.equal(g.canDecide('b', 900, r(50)), true);
});

test('agent text renders inert: hidden and bidi characters become visible tokens', () => {
  assert.deepEqual(inertLines('safe‮evil​!\nline two'), ['safe⟨U+202E⟩evil⟨U+200B⟩!', 'line two']);
  assert.deepEqual(inertLines('a bㅤc️d\u0007'), ['a⟨U+00A0⟩b⟨U+3164⟩c⟨U+FE0F⟩d⟨U+0007⟩']);
  assert.deepEqual(inertLines('tab\tok space'), ['tab\tok space']);
  assert.deepEqual(inertLines('tag\u{E0041}'), ['tag⟨U+E0041⟩']);
});
