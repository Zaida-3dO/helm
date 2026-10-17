import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RenderOffloader } from './offload.ts';
import type { RenderRequest, WorkerLike } from './offload.ts';

/** A worker the test drives by hand. */
class FakeWorker implements WorkerLike {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  terminated = false;
  requests: RenderRequest[] = [];
  postMessage(message: unknown): void {
    this.requests.push(message as RenderRequest);
  }
  terminate(): void {
    this.terminated = true;
  }
  reply(index: number, html: string): void {
    this.onmessage?.({ data: { id: this.requests[index]?.id, html } });
  }
}

/** A manual clock: timers fire only when the test says so. */
function clock() {
  const timers = new Map<number, { fn: () => void; ms: number }>();
  let next = 1;
  return {
    setTimer: (fn: () => void, ms: number) => {
      timers.set(next, { fn, ms });
      return next++;
    },
    clearTimer: (h: unknown) => void timers.delete(h as number),
    fireAll: () => {
      for (const [id, t] of [...timers]) {
        timers.delete(id);
        t.fn();
      }
    },
    count: () => timers.size,
    lastMs: () => [...timers.values()].at(-1)?.ms,
  };
}

function setup(extra: { cacheEntries?: number } = {}) {
  const workers: FakeWorker[] = [];
  const c = clock();
  const offloader = new RenderOffloader({
    createWorker: () => {
      const w = new FakeWorker();
      workers.push(w);
      return w;
    },
    setTimer: c.setTimer,
    clearTimer: c.clearTimer,
    ...extra,
  });
  return { offloader, workers, c };
}

test('a render posts to the worker and resolves with its html, defaulting to a 2 s timeout', async () => {
  const { offloader, workers, c } = setup();
  const p = offloader.render('# hi', { interactive: true });
  assert.equal(workers.length, 1);
  assert.deepEqual(workers[0]?.requests[0]?.options, { interactive: true });
  assert.equal(workers[0]?.requests[0]?.text, '# hi');
  assert.equal(c.lastMs(), 2000);
  workers[0]?.reply(0, '<h1>hi</h1>');
  assert.equal(await p, '<h1>hi</h1>');
  assert.equal(c.count(), 0, 'the timer is cleared on success');
});

test('the same text is served from the cache without a second worker request', async () => {
  const { offloader, workers } = setup();
  const first = offloader.render('same');
  workers[0]?.reply(0, '<p>same</p>');
  await first;
  assert.equal(await offloader.render('same'), '<p>same</p>');
  assert.equal(workers[0]?.requests.length, 1);
  assert.equal(offloader.cached('same'), '<p>same</p>');
});

test('options are part of the cache key', async () => {
  const { offloader, workers } = setup();
  const a = offloader.render('x', { interactive: false });
  workers[0]?.reply(0, 'plain');
  await a;
  const b = offloader.render('x', { interactive: true });
  assert.equal(workers[0]?.requests.length, 2);
  workers[0]?.reply(1, 'interactive');
  assert.equal(await b, 'interactive');
});

test('streaming snapshots are not cached', async () => {
  const { offloader, workers } = setup();
  const p = offloader.render('partial', { streaming: true });
  workers[0]?.reply(0, 'x');
  await p;
  assert.equal(offloader.cached('partial', { streaming: true }), undefined);
  offloader.remember('partial', { streaming: true }, 'y');
  assert.equal(offloader.cached('partial', { streaming: true }), undefined);
});

test('a timeout rejects, terminates the worker, and the next render uses a fresh one', async () => {
  const { offloader, workers, c } = setup();
  const stuck = offloader.render('slow');
  c.fireAll();
  await assert.rejects(stuck, /timed out after 2000 ms/);
  assert.equal(workers[0]?.terminated, true);

  const next = offloader.render('fast');
  assert.equal(workers.length, 2);
  assert.equal(offloader.workersCreated, 2);
  workers[1]?.reply(0, '<p>fast</p>');
  assert.equal(await next, '<p>fast</p>');
});

test('a reset fails every request in flight and ignores a late reply', async () => {
  const { offloader, workers, c } = setup();
  const a = offloader.render('a');
  const b = offloader.render('b');
  c.fireAll();
  await assert.rejects(a, /timed out/);
  await assert.rejects(b, /timed out/);
  // The terminated worker answering afterwards must not resolve or cache anything.
  workers[0]?.onmessage?.({ data: { id: 1, html: 'late' } });
  assert.equal(offloader.cached('a'), undefined);
});

test('a worker error rejects and resets', async () => {
  const { offloader, workers } = setup();
  const p = offloader.render('boom');
  workers[0]?.onerror?.(new Error('x'));
  await assert.rejects(p, /worker failed/);
  assert.equal(workers[0]?.terminated, true);
});

test('failing to create a worker rejects instead of throwing', async () => {
  const offloader = new RenderOffloader({
    createWorker: () => {
      throw new Error('no workers here');
    },
  });
  await assert.rejects(offloader.render('x'), /no workers here/);
});

test('the cache evicts the least recently used entry', async () => {
  const { offloader, workers } = setup({ cacheEntries: 2 });
  for (const [i, t] of ['a', 'b'].entries()) {
    const p = offloader.render(t);
    workers[0]?.reply(i, t.toUpperCase());
    await p;
  }
  assert.equal(offloader.cached('a'), 'A'); // touch a: b is now the oldest
  const p = offloader.render('c');
  workers[0]?.reply(2, 'C');
  await p;
  assert.equal(offloader.cached('b'), undefined);
  assert.equal(offloader.cached('a'), 'A');
  assert.equal(offloader.cached('c'), 'C');
});
