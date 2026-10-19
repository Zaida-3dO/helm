import { test } from 'node:test';
import assert from 'node:assert/strict';
import { classifyCadence, MotionPolicy } from './cadence.ts';

const run = (ms: number, n = 30) => Array<number>(n).fill(ms);

test('a 32 Hz display is low, 60 Hz and faster are normal', () => {
  assert.equal(classifyCadence(run(31.25)), 'low');
  assert.equal(classifyCadence(run(25)), 'low');
  assert.equal(classifyCadence(run(16.7)), 'normal');
  assert.equal(classifyCadence(run(8.3)), 'normal');
});

test('the median decides: a few slow frames on a fast display do not make it low', () => {
  assert.equal(classifyCadence([...run(16.7, 25), ...run(60, 10)]), 'normal');
  assert.equal(classifyCadence([...run(31.25, 25), ...run(8, 10)]), 'low');
});

test('throttled frames and short runs are inconclusive', () => {
  assert.equal(classifyCadence(run(250)), null, 'a throttled or hidden window says nothing about the display');
  assert.equal(classifyCadence(run(31.25, 5)), null);
  assert.equal(classifyCadence([0, -1, ...run(16.7, 11)]), null);
});

test('the policy follows the measured cadence, a forced one wins, and listeners hear changes only', () => {
  const p = new MotionPolicy();
  let heard = 0;
  p.subscribe(() => heard++);
  assert.equal(p.low, false);
  p.setCadence('low');
  assert.equal(p.low, true);
  p.setCadence('low');
  assert.equal(heard, 1, 'no change, no event');
  p.force('normal');
  assert.equal(p.low, false);
  p.force('auto');
  assert.equal(p.low, true);
  assert.equal(heard, 3);
});
