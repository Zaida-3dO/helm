import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventBus } from '../api/events.ts';
import { followSendShortcut, parseSendShortcut } from './sendShortcut.ts';

const apiReturning = (v: unknown) => (async () => v) as never;

test('a missing or unreadable value is the default; both real values are kept', () => {
  assert.equal(parseSendShortcut(null), 'mod-enter');
  assert.equal(parseSendShortcut('ctrl-enter'), 'mod-enter');
  assert.equal(parseSendShortcut('enter'), 'enter');
  assert.equal(parseSendShortcut('mod-enter'), 'mod-enter');
});

test('the stored value is applied at once and a preferences event changes it live', async () => {
  const bus = new EventBus();
  const seen: string[] = [];
  followSendShortcut(apiReturning('enter'), bus, (s) => seen.push(s));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(seen, ['enter']);
  bus.dispatch({ kind: 'preferences', changed: { sendShortcut: 'mod-enter' } });
  assert.deepEqual(seen, ['enter', 'mod-enter']);
  // Resetting (null) returns to the default; other keys are ignored.
  bus.dispatch({ kind: 'preferences', changed: { sendShortcut: null } });
  bus.dispatch({ kind: 'preferences', changed: { theme: 'dark' } });
  assert.deepEqual(seen, ['enter', 'mod-enter', 'mod-enter']);
});

test('stopping detaches the listener', async () => {
  const bus = new EventBus();
  const seen: string[] = [];
  const stop = followSendShortcut(apiReturning(null), bus, (s) => seen.push(s));
  stop();
  bus.dispatch({ kind: 'preferences', changed: { sendShortcut: 'enter' } });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(seen, []);
});
