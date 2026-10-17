import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createApi } from '../api/client.ts';
import { EventBus } from '../api/events.ts';
import { Flushers, installQuitHandling } from './quit.ts';

function harness() {
  const ops: string[] = [];
  const api = createApi(async (_cmd, payload) => {
    ops.push(String(payload.op));
    return null;
  });
  const bus = new EventBus();
  const flushers = new Flushers();
  installQuitHandling(bus, api, flushers);
  return { ops, bus, flushers };
}

const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

test('quitting flushes first, then reports quit_ready', async () => {
  const { ops, bus, flushers } = harness();
  const order: string[] = [];
  flushers.add(async () => {
    await tick(10);
    order.push('flushed');
  });
  bus.dispatch({ kind: 'quitting' });
  await tick(50);
  order.push(...ops);
  assert.deepEqual(order, ['flushed', 'quit_ready']);
});

test('a hung flusher does not hold the quit past the budget', async () => {
  const { ops, flushers } = harness();
  flushers.add(() => new Promise<void>(() => {}));
  const start = Date.now();
  await flushers.flushAll(30);
  assert.ok(Date.now() - start < 1000);
  assert.deepEqual(ops, []);
});

test('closing flushes without acknowledging a quit', async () => {
  const { ops, bus, flushers } = harness();
  let flushed = 0;
  flushers.add(() => {
    flushed += 1;
  });
  bus.dispatch({ kind: 'closing' });
  await tick(20);
  assert.equal(flushed, 1);
  assert.deepEqual(ops, []);
});
