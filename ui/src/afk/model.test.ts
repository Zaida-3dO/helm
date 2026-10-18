import { test } from 'node:test';
import assert from 'node:assert/strict';
import { clampInt, PresenceThrottle, sourceLabel, toggleWait } from './model.ts';

test('wait toggle stores null and restores 30', () => {
  assert.equal(toggleWait(12, false), null);
  assert.equal(toggleWait(null, true), 30);
  assert.equal(toggleWait(12, true), 12);
});

test('clampInt clamps and falls back', () => {
  assert.equal(clampInt('3', 5, 240, 30), 5);
  assert.equal(clampInt('999', 5, 240, 30), 240);
  assert.equal(clampInt('abc', 5, 240, 30), 30);
  assert.equal(clampInt('', 0, 600, 7), 7);
  assert.equal(clampInt('45', 5, 240, 30), 45);
});

test('presence throttle reports input at most once per gap', () => {
  const t = new PresenceThrottle(20_000);
  assert.equal(t.input(0), true);
  assert.equal(t.input(19_999), false);
  assert.equal(t.input(20_000), true);
});

test('source labels match the Settings wording', () => {
  assert.equal(sourceLabel('screenLock'), 'screen locked');
  assert.equal(sourceLabel('unanswered'), 'question waiting');
});
