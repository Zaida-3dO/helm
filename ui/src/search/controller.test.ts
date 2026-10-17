import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  DEBOUNCE_MS,
  flatRows,
  moveHighlight,
  movePage,
  normaliseQuery,
  statusLine,
  UniversalSearch,
  type Timers,
} from './controller.ts';
import { GROUP_LIMIT, type Row, type SearchSource, type SourceId } from './sources.ts';

/** Timers the test advances by hand. */
class Clock implements Timers {
  now = 0;
  private next = 1;
  private pending = new Map<number, { at: number; f: () => void }>();
  set = (f: () => void, ms: number): number => {
    const id = this.next++;
    this.pending.set(id, { at: this.now + ms, f });
    return id;
  };
  clear = (h: unknown): void => {
    this.pending.delete(h as number);
  };
  advance(ms: number): void {
    this.now += ms;
    for (const [id, t] of [...this.pending].sort((a, b) => a[1].at - b[1].at)) {
      if (t.at <= this.now && this.pending.has(id)) {
        this.pending.delete(id);
        t.f();
      }
    }
  }
}

const row = (source: SourceId, key: string): Row => ({ id: `${source}:${key}`, title: key, open: () => undefined });
const flush = () => new Promise((r) => setImmediate(r));

function source(id: SourceId, run: SearchSource['run'], over: Partial<SearchSource> = {}): SearchSource {
  return { id, minChars: 1, timeoutMs: 5_000, run, ...over };
}
const answers = (id: SourceId, keys: string[]) => source(id, async () => keys.map((k) => row(id, k)));

test('a query waits 120 ms before any source runs, and typing again restarts the wait', async () => {
  const clock = new Clock();
  const calls: string[] = [];
  const s = new UniversalSearch([source('chats', async (q) => (calls.push(q), []))], clock);
  s.update('fi');
  clock.advance(DEBOUNCE_MS - 1);
  s.update('fix');
  clock.advance(DEBOUNCE_MS - 1);
  assert.deepEqual(calls, [], 'the second keystroke restarted the wait');
  clock.advance(1);
  await flush();
  assert.deepEqual(calls, ['fix']);
});

test('the dialog says searching as soon as the user types, then done when every source settled', async () => {
  const clock = new Clock();
  const s = new UniversalSearch([answers('chats', ['a']), answers('files', ['f'])], clock);
  s.update('x');
  assert.equal(s.snapshot().status, 'searching');
  assert.equal(s.snapshot().groups.chats?.status, 'searching');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(s.snapshot().status, 'done');
  assert.deepEqual(s.snapshot().groups.chats?.rows.map((r) => r.id), ['chats:a']);
  assert.deepEqual(s.snapshot().groups.files?.rows.map((r) => r.id), ['files:f']);
});

test('a source that rejects settles as an error while the others still show their rows', async () => {
  const clock = new Clock();
  const bad = source('files', async () => {
    throw new Error('quick open is down');
  });
  const s = new UniversalSearch([answers('chats', ['a']), bad], clock);
  s.update('x');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(s.snapshot().status, 'done');
  assert.deepEqual([s.snapshot().groups.files?.status, s.snapshot().groups.files?.error], ['error', 'quick open is down']);
  assert.equal(s.snapshot().groups.chats?.rows.length, 1);
});

test('a source that throws before returning a promise still settles', async () => {
  const clock = new Clock();
  const bad = source('files', () => {
    throw new Error('sync boom');
  });
  const s = new UniversalSearch([answers('chats', ['a']), bad], clock);
  s.update('x');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(s.snapshot().groups.files?.status, 'error');
  assert.equal(s.snapshot().status, 'done');
});

test('a source that never answers is settled as failed at its deadline and cannot hang the search', async () => {
  const clock = new Clock();
  const hang = source('files', () => new Promise<Row[]>(() => undefined), { timeoutMs: 3_000 });
  const s = new UniversalSearch([answers('chats', ['a']), hang], clock);
  s.update('x');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(s.snapshot().status, 'searching', 'files is still pending');
  assert.equal(s.snapshot().groups.chats?.status, 'done');
  clock.advance(3_000);
  await flush();
  assert.equal(s.snapshot().status, 'done');
  assert.equal(s.snapshot().groups.files?.status, 'error');
});

test('a slow older answer never replaces a newer search, and the older run is aborted', async () => {
  const clock = new Clock();
  const resolvers: Array<(r: Row[]) => void> = [];
  const signals: AbortSignal[] = [];
  const slow = source('chats', (_q, signal) => {
    signals.push(signal);
    return new Promise<Row[]>((res) => resolvers.push(res));
  });
  const s = new UniversalSearch([slow], clock);
  s.update('old');
  clock.advance(DEBOUNCE_MS);
  s.update('new');
  assert.equal(signals[0]!.aborted, true, 'the old run is told to stop');
  clock.advance(DEBOUNCE_MS);
  resolvers[1]!([row('chats', 'new-result')]);
  await flush();
  resolvers[0]!([row('chats', 'old-result')]);
  await flush();
  assert.deepEqual(s.snapshot().groups.chats?.rows.map((r) => r.id), ['chats:new-result']);
  assert.equal(s.snapshot().query, 'new');
});

test('an old answer is dropped even when it arrives before the new query has run', async () => {
  const clock = new Clock();
  let resolve: (r: Row[]) => void = () => undefined;
  const s = new UniversalSearch([source('chats', () => new Promise<Row[]>((res) => (resolve = res)))], clock);
  s.update('first');
  clock.advance(DEBOUNCE_MS);
  s.update('second');
  resolve([row('chats', 'first-result')]);
  await flush();
  assert.equal(s.snapshot().groups.chats?.status, 'searching');
  assert.deepEqual(s.snapshot().groups.chats?.rows, []);
});

test('an empty query clears at once and cancels a pending run', () => {
  const clock = new Clock();
  const calls: string[] = [];
  const s = new UniversalSearch([source('chats', async (q) => (calls.push(q), []))], clock);
  s.update('abc');
  s.update('   ');
  clock.advance(500);
  assert.deepEqual(calls, []);
  assert.equal(s.snapshot().status, 'idle');
});

test('a source that needs more characters stays idle for a shorter query', async () => {
  const clock = new Clock();
  const calls: string[] = [];
  const board = source('board', async (q) => (calls.push(q), []), { minChars: 2 });
  const s = new UniversalSearch([answers('chats', ['a']), board], clock);
  s.update('x');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.deepEqual(calls, []);
  assert.equal(s.snapshot().groups.board?.status, 'idle');
  s.update('xy');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.deepEqual(calls, ['xy']);
});

test('a group is cut to its row limit', async () => {
  const clock = new Clock();
  const many = Array.from({ length: GROUP_LIMIT.files + 15 }, (_, i) => `f${i}`);
  const s = new UniversalSearch([answers('files', many)], clock);
  s.update('x');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(s.snapshot().groups.files?.rows.length, GROUP_LIMIT.files);
});

test('disposing drops a pending search and late answers', async () => {
  const clock = new Clock();
  const calls: string[] = [];
  const s = new UniversalSearch([source('chats', async (q) => (calls.push(q), []))], clock);
  s.update('x');
  s.dispose();
  clock.advance(500);
  assert.deepEqual(calls, []);
});

test('rows are listed Chats then Files, and Files first when the query looks like a path', async () => {
  const clock = new Clock();
  const s = new UniversalSearch([answers('chats', ['c1']), answers('files', ['f1'])], clock);
  s.update('notes');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.deepEqual(flatRows(s.snapshot()).map((r) => r.row.id), ['chats:c1', 'files:f1']);
  s.update('src/main.ts');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.deepEqual(flatRows(s.snapshot()).map((r) => r.row.id), ['files:f1', 'chats:c1']);
});

test('queries are trimmed to 12 words and 512 bytes', () => {
  const many = Array.from({ length: 20 }, (_, i) => `w${i}`).join('  ');
  assert.equal(normaliseQuery(many).split(' ').length, 12);
  assert.equal(normaliseQuery('  a   b  '), 'a b');
  const long = normaliseQuery('é'.repeat(400));
  assert.ok(new TextEncoder().encode(long).length <= 512);
  assert.ok(long.length > 200);
});

test('Up and Down wrap; Page keys move ten and stop at the ends', () => {
  assert.equal(moveHighlight(0, -1, 3), 2);
  assert.equal(moveHighlight(2, 1, 3), 0);
  assert.equal(moveHighlight(0, 1, 0), 0);
  assert.equal(movePage(0, 1, 25), 10);
  assert.equal(movePage(20, 1, 25), 24);
  assert.equal(movePage(3, -1, 25), 0);
  assert.equal(movePage(0, 1, 0), 0);
});

test('the status line reports typing, searching and the number of results', async () => {
  const clock = new Clock();
  const s = new UniversalSearch([answers('chats', ['a', 'b'])], clock);
  assert.equal(statusLine(s.snapshot()), 'Type to search.');
  s.update('x');
  assert.equal(statusLine(s.snapshot()), 'Searching…');
  clock.advance(DEBOUNCE_MS);
  await flush();
  assert.equal(statusLine(s.snapshot()), '2 results.');
});
