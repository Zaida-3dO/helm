import { test } from 'node:test';
import assert from 'node:assert/strict';
import { BACKLOG_CAP, COMMIT_MS, FINISH_WINDOW_MS, Pacer, STEADY_GAP_MS, wordEnd } from './pacer.ts';

const words = (n: number) => Array.from({ length: n }, (_, i) => `w${i}`).join(' ');

/** A pacer that has shown a first burst, so the next push is paced. */
function started(now = 0): Pacer {
  const p = new Pacer();
  p.push('Hi ', now, false);
  return p;
}

test('the first burst shows at once', () => {
  const p = new Pacer();
  const text = words(50);
  p.push(text, 0, false);
  assert.equal(p.text, text);
  assert.equal(p.tick(1), null, 'nothing left to reveal');
});

test('later bursts are spread over the steady gap, ending on word boundaries', () => {
  const p = started();
  const text = 'Hi ' + words(60);
  p.push(text, 1000, false);
  const half = p.tick(1000 + STEADY_GAP_MS / 2);
  assert.ok(half !== null);
  assert.ok(half.length > 3 && half.length < text.length, 'partly revealed half way through');
  assert.ok(text[half.length] === ' ' || text[half.length - 1] === ' ', 'stops at a word boundary');
  assert.equal(p.tick(1000 + STEADY_GAP_MS), text, 'fully revealed when the window closes');
});

test('commits at most every COMMIT_MS', () => {
  const p = started();
  p.push('Hi ' + words(200), 10, false);
  assert.notEqual(p.tick(50), null);
  assert.equal(p.tick(50 + COMMIT_MS - 1), null);
  assert.notEqual(p.tick(50 + COMMIT_MS), null);
});

test('done reveals the rest over the finish window', () => {
  const p = started();
  const text = 'Hi ' + words(100);
  p.push(text, 0, false);
  p.tick(40);
  p.push(text, 50, true);
  const mid = p.tick(50 + FINISH_WINDOW_MS / 2);
  assert.ok(mid !== null && mid.length < text.length);
  assert.equal(p.tick(50 + FINISH_WINDOW_MS), text);
  assert.ok(p.finished);
});

test('the backlog is capped', () => {
  const p = started();
  const text = 'Hi ' + 'x '.repeat(BACKLOG_CAP);
  p.push(text, 0, false);
  assert.ok(text.length - p.text.length <= BACKLOG_CAP);
  assert.ok(p.text.length >= text.length - BACKLOG_CAP);
});

test('hidden documents and rewritten text skip pacing', () => {
  const p = started();
  p.push('Hi hello there', 0, false, true);
  assert.equal(p.text, 'Hi hello there');
  const q = started();
  q.push('Hi abc def', 0, false);
  q.push('zzz', 10, false);
  assert.equal(q.text, 'zzz');
});

test('wordEnd', () => {
  assert.equal(wordEnd('ab cd', 1), 2);
  assert.equal(wordEnd('ab cd', 3), 3);
  assert.equal(wordEnd('ab cd', 4), 5);
  assert.equal(wordEnd('ab', 0), 0);
});
