import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fitWithin } from './fakeThumb.ts';

test('a large image is fitted into 480 by 360 with its shape kept; a small one is not scaled up', () => {
  assert.deepEqual(fitWithin(1920, 1080), { width: 480, height: 270 });
  assert.deepEqual(fitWithin(1000, 2000), { width: 180, height: 360 });
  assert.deepEqual(fitWithin(100, 50), { width: 100, height: 50 });
});
