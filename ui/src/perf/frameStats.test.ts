import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FrameSampler, MAX_INTERVALS, summarise } from './frameStats.ts';

test('summarise: fps, average, nearest-rank p95, worst and slow frames', () => {
  // 19 frames of 10 ms and one of 60 ms over 250 ms.
  const xs = [...Array<number>(19).fill(10), 60];
  const s = summarise(xs, 250)!;
  assert.equal(s.frames, 20);
  assert.equal(s.fps, 80);
  assert.equal(s.avgMs, 12.5);
  // Nearest rank: ceil(0.95 × 20) = 19th smallest, which is still 10.
  assert.equal(s.p95Ms, 10);
  assert.equal(s.worstMs, 60);
  assert.equal(s.slow, 1);
});

test('summarise: p95 picks the top value once it is 5% of the sample', () => {
  const s = summarise([10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 40, 50], 230)!;
  assert.equal(s.p95Ms, 40);
  // 50 is not over 50 ms.
  assert.equal(s.slow, 0);
});

test('summarise ignores non-positive intervals and returns null with nothing to report', () => {
  assert.equal(summarise([], 500), null);
  assert.equal(summarise([0, -3], 500), null);
  assert.equal(summarise([0, 20], 500)!.frames, 1);
});

test('the sampler publishes every 500 ms', () => {
  const s = new FrameSampler();
  const out = [];
  for (let t = 0; t <= 1000; t += 31.25) {
    const r = s.frame(t);
    if (r) out.push(r);
  }
  assert.equal(out.length, 2);
  assert.equal(out[0]!.frames, 16);
  assert.equal(Math.round(out[0]!.fps), 32);
  assert.equal(out[0]!.avgMs, 31.25);
});

test('the sampler publishes early at 1024 intervals', () => {
  const s = new FrameSampler();
  let published = null;
  let n = 0;
  for (let t = 0; !published; t += 0.1) {
    published = s.frame(t);
    n++;
  }
  assert.equal(published.frames, MAX_INTERVALS);
  assert.equal(n, MAX_INTERVALS + 1);
});

test('reset starts a fresh window: the hidden gap is not counted as a frame', () => {
  const s = new FrameSampler();
  s.frame(0);
  s.frame(16);
  s.reset();
  // Back after 10 s hidden.
  s.frame(10_016);
  let r = null;
  for (let t = 10_032; !r; t += 16) r = s.frame(t);
  assert.equal(r.worstMs, 16);
});
