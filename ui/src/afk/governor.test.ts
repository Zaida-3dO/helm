import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bandLabel, thresholdsError } from './governor.ts';

const s = { enabled: true, selectivePct: 60, windDownPct: 85, stopPct: 95, atStop: 'stop' as const };

test('thresholds must strictly rise', () => {
  assert.equal(thresholdsError(s), null);
  assert.notEqual(thresholdsError({ ...s, windDownPct: 95 }), null);
  assert.notEqual(thresholdsError({ ...s, selectivePct: 85 }), null);
  assert.notEqual(thresholdsError({ ...s, stopPct: 50 }), null);
});

test('band labels', () => {
  assert.equal(bandLabel('windDown'), 'winding down');
  assert.equal(bandLabel('unknown'), 'no fresh usage reading');
});
