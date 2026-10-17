import { test } from 'node:test';
import assert from 'node:assert/strict';
import { AckBatcher, ACK_EVERY, clampHeight, nextTabState, routeKey, SeqFilter } from './model.ts';

const k = (code: string, mods: { ctrl?: boolean; shift?: boolean; alt?: boolean } = {}, key = '') => ({
  key,
  code,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
  metaKey: false,
});

test('the dock height stays between 120 px and the pane minus 140 px', () => {
  assert.equal(clampHeight(50, 800), 120);
  assert.equal(clampHeight(300, 800), 300);
  assert.equal(clampHeight(9999, 800), 660);
  assert.equal(clampHeight(300, 200), 120, 'a tiny pane still gets the minimum');
});

test('after a reattach, chunks the replay already covered are dropped', () => {
  const f = new SeqFilter();
  f.reset(5);
  assert.equal(f.accept(5), false);
  assert.equal(f.accept(6), true);
  assert.equal(f.accept(6), false, 'a duplicate is dropped');
  assert.equal(f.accept(8), true);
});

test('acks are batched: one call per 64 Ki units, the rest on flush', () => {
  const sent: number[] = [];
  const a = new AckBatcher((n) => sent.push(n));
  assert.equal(a.wrote(ACK_EVERY - 1), false);
  assert.equal(a.wrote(1), true);
  a.wrote(10);
  a.flush();
  a.flush();
  assert.deepEqual(sent, [ACK_EVERY, 10]);
});

test('Ctrl+C and plain Ctrl chords go to the shell; dock commands are Ctrl+Shift; toggles reach the app', () => {
  assert.equal(routeKey(k('KeyC', { ctrl: true }, 'c')), 'shell');
  assert.equal(routeKey(k('KeyW', { ctrl: true }, 'w')), 'shell');
  assert.equal(routeKey(k('KeyC', { ctrl: true, shift: true })), 'copy');
  assert.equal(routeKey(k('KeyT', { ctrl: true, shift: true })), 'new');
  assert.equal(routeKey(k('KeyF', { ctrl: true, shift: true })), 'find');
  assert.equal(routeKey(k('Digit3', { ctrl: true, shift: true })), 'select:3');
  assert.equal(routeKey(k('Backquote', { ctrl: true })), 'app');
  assert.equal(routeKey(k('KeyJ', { ctrl: true }, 'j')), 'app');
  assert.equal(routeKey(k('KeyA', {}, 'a')), 'shell');
});

test('tab dots: background output marks unseen, seeing clears it, exit sticks', () => {
  assert.equal(nextTabState('idle', 'output', false), 'unseen');
  assert.equal(nextTabState('idle', 'output', true), 'idle');
  assert.equal(nextTabState('unseen', 'seen', true), 'idle');
  assert.equal(nextTabState('unseen', 'exit', false), 'exited');
  assert.equal(nextTabState('exited', 'output', false), 'exited');
});
