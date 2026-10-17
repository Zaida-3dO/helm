import { test } from 'node:test';
import assert from 'node:assert/strict';
import { BACKLOG_CAP, COMMIT_MS, FINISH_WINDOW_MS, FIRST_GAP_MS, Pacer, STEADY_GAP_MS, wordEnd } from './pacer.ts';

const words = (n: number) => Array.from({ length: n }, (_, i) => `w${i}`).join(' ');

test('the first burst is spread over the first-gap assumption, ending on word boundaries', () => {
  const p = new Pacer();
  const text = words(50);
  p.push(text, 0, false);
  const half = p.tick(FIRST_GAP_MS / 2);
  assert.ok(half !== null);
  assert.ok(half.length > 0 && half.length < text.length, 'partly revealed half way through');
  assert.ok(text[half.length] === ' ' || text[half.length - 1] === ' ', 'stops at a word boundary');
  assert.equal(p.tick(FIRST_GAP_MS), text, 'fully revealed when the window closes');
});

test('later bursts use the steady gap', () => {
  const p = new Pacer();
  p.push('a b', 0, false);
  p.tick(FIRST_GAP_MS);
  const more = 'a b ' + words(40);
  p.push(more, 1000, false);
  assert.equal(p.tick(1000 + STEADY_GAP_MS), more);
  // The same burst would still be partial at that point if it were the first.
  const q = new Pacer();
  q.push(more, 0, false);
  assert.notEqual(q.tick(STEADY_GAP_MS), more);
});

test('commits at most every COMMIT_MS', () => {
  const p = new Pacer();
  p.push(words(200), 0, false);
  assert.notEqual(p.tick(100), null);
  assert.equal(p.tick(100 + COMMIT_MS - 1), null);
  assert.notEqual(p.tick(100 + COMMIT_MS), null);
});

test('done reveals the rest over the finish window', () => {
  const p = new Pacer();
  const text = words(100);
  p.push(text, 0, false);
  p.tick(40);
  p.push(text, 50, true);
  const mid = p.tick(50 + FINISH_WINDOW_MS / 2);
  assert.ok(mid !== null && mid.length < text.length);
  assert.equal(p.tick(50 + FINISH_WINDOW_MS), text);
  assert.ok(p.finished);
});

test('the backlog is capped', () => {
  const p = new Pacer();
  const text = 'x '.repeat(BACKLOG_CAP);
  p.push(text, 0, false);
  assert.ok(text.length - p.text.length <= BACKLOG_CAP);
  assert.ok(p.text.length >= text.length - BACKLOG_CAP);
});

test('hidden documents and rewritten text skip pacing', () => {
  const p = new Pacer();
  p.push('hello there', 0, false, true);
  assert.equal(p.text, 'hello there');
  const q = new Pacer();
  q.push('abc def', 0, false);
  q.tick(FIRST_GAP_MS);
  q.push('zzz', 10, false);
  assert.equal(q.text, 'zzz');
});

test('wordEnd', () => {
  assert.equal(wordEnd('ab cd', 1), 2);
  assert.equal(wordEnd('ab cd', 3), 3);
  assert.equal(wordEnd('ab cd', 4), 5);
  assert.equal(wordEnd('ab', 0), 0);
});
