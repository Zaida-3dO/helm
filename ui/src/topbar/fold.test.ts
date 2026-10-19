import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MIN_TITLE_ROOM, UNFOLD_SLACK, shouldFold, titleRoom } from './fold.ts';

test('the title room is the breadcrumb width, less any overflow of the bar', () => {
  assert.equal(titleRoom(225, 1120, 1120), 225);
  assert.equal(titleRoom(0, 1000, 740), -260);
  assert.equal(titleRoom(40, 739, 740), 40, 'no overflow counts as none');
});

test('an unfolded row folds once the title would get less than the minimum', () => {
  assert.equal(shouldFold(false, MIN_TITLE_ROOM, 360, 90), false);
  assert.equal(shouldFold(false, MIN_TITLE_ROOM - 1, 360, 90), true);
  assert.equal(shouldFold(false, -260, 360, 90), true);
});

test('a folded row unfolds only when the inline buttons fit with slack to spare', () => {
  // Unfolding swaps a 90 px trigger for a 360 px row: the title loses 270 px.
  const edge = MIN_TITLE_ROOM + UNFOLD_SLACK + 270;
  assert.equal(shouldFold(true, edge, 360, 90), false);
  assert.equal(shouldFold(true, edge - 1, 360, 90), true);
  // Just past the fold threshold is not enough to unfold: no flapping.
  assert.equal(shouldFold(true, MIN_TITLE_ROOM + 270, 360, 90), true);
});

test('a folded row whose inline width was never measured stays folded', () => {
  assert.equal(shouldFold(true, 5000, 0, 90), true);
});
