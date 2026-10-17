import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventBus } from './events.ts';

test('events route by kind and handlers can unsubscribe', () => {
  const bus = new EventBus();
  const seen: string[] = [];
  const off = bus.on('refresh', (e) => seen.push(`refresh:${String(e.chatId)}`));
  bus.on('quitting', () => seen.push('quitting'));
  bus.dispatch({ kind: 'refresh', chatId: 'c1' });
  bus.dispatch({ kind: 'quitting' });
  bus.dispatch({ kind: 'other' });
  off();
  bus.dispatch({ kind: 'refresh', chatId: 'c2' });
  assert.deepEqual(seen, ['refresh:c1', 'quitting']);
});

test('malformed payloads are dropped, and a throwing handler does not stop the rest', () => {
  const bus = new EventBus();
  const seen: string[] = [];
  bus.on('x', () => {
    throw new Error('boom');
  });
  bus.on('x', () => seen.push('second'));
  const quiet = console.error;
  console.error = () => {};
  try {
    bus.dispatch(null);
    bus.dispatch({ kind: 3 });
    bus.dispatch('x');
    bus.dispatch({ kind: 'x' });
  } finally {
    console.error = quiet;
  }
  assert.deepEqual(seen, ['second']);
});
