import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError, type Api } from '../api/client.ts';
import type { BoardEntry, FleetAssignment } from '../api/board.ts';
import { boardSource } from '../search/boardSource.ts';
import { itemContextBlock, MAX_CONTEXT_BYTES, mentionsItem, withItemContext } from './context.ts';
import { columnCount, groupFleet, holdersFor, itemIdPrefix, mergeEntries, normaliseBoardQuery, safeHttpUrl } from './model.ts';
import { BoardReader } from './reader.ts';
import { TasksStore, visibleProjects } from './store.ts';

// ---- a scripted board ---------------------------------------------------------------------

type Params = Record<string, unknown> & { action: string; requestId: string };
type Script = (p: Params) => unknown | Promise<unknown>;

/** An `Api` whose `board_read` runs `script`, recording every request and cancel. */
function scripted(script: Script, connection = { url: 'https://board.example', hasToken: true }) {
  const reads: Params[] = [];
  const cancels: string[] = [];
  const api = (async (op: string, args?: unknown) => {
    if (op === 'board_connection_get') return connection;
    if (op === 'board_cancel') {
      cancels.push((args as { requestId: string }).requestId);
      return { cancelled: true };
    }
    if (op === 'board_read') {
      const p = args as Params;
      reads.push(p);
      return { action: p.action, data: await script(p) };
    }
    throw new Error(`unexpected op ${op}`);
  }) as Api;
  return { api, reads, cancels };
}

const entry = (id: string, over: Partial<BoardEntry> = {}): BoardEntry => ({
  item: { id, title: `Task ${id}`, state: 'executing', headline: null, priority: 'P1' },
  column: 'in_progress',
  assignments: [],
  subtasks: null,
  ...over,
});

const section = (ids: string[], total: number, nextCursor: string | null = null) => ({ entries: ids.map((i) => entry(i)), total, nextCursor });

const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

// ---- the small rules ----------------------------------------------------------------------

test('links draw as links only when plain http(s); search text, id prefixes and counts follow the board rules', () => {
  const links: [string, boolean][] = [
    ['https://github.com/o/r/pull/1', true],
    ['http://localhost:3000/x', true],
    ['https://user:pw@host.example/', false],
    ['javascript:alert(1)', false],
    ['file:///C:/secret', false],
    ['not a url', false],
  ];
  for (const [url, ok] of links) assert.equal(safeHttpUrl(url) !== null, ok, url);

  const queries: [string, string | null][] = [
    ['  merge   queue  ', 'merge queue'],
    ['a', null],
    [' \u0007x\u0000 ', null],
    ['ab', 'ab'],
    ['x'.repeat(300), 'x'.repeat(200)],
  ];
  for (const [raw, want] of queries) assert.equal(normaliseBoardQuery(raw), want, raw);

  const prefixes: [string, string | null][] = [
    ['5C9E2A41', '5c9e2a41'],
    ['5c9e2a41-0f0b', '5c9e2a41-0f0b'],
    ['5c9e2a41-0f0b-4d6e-9d1f', '5c9e2a41-0f0b-4d6e-9d1f'],
    ['5c9e2a41-0f0b-4d6e-9d1f-0a7c1e3b9a10', '5c9e2a41-0f0b-4d6e-9d1f-0a7c1e3b9a10'],
    ['5c9e2a410f', null],
    ['5c9e2a4', null],
    ['5c9e2a41-0f', null],
    ['merge queue', null],
  ];
  for (const [t, want] of prefixes) assert.equal(itemIdPrefix(t), want, t);

  assert.equal(columnCount(0, null, false), '…');
  assert.equal(columnCount(30, 41, true), '30 / 41');
  assert.equal(columnCount(5, null, true), '5 loaded');
  const merged = mergeEntries([entry('a'), entry('b')], [entry('b'), entry('c')]);
  assert.deepEqual(merged.map((e) => e.item.id), ['a', 'b', 'c']);
});

test('a card names its holders only when the fleet snapshot matches exactly', () => {
  const holder = { holderId: 'builder-one', displayName: 'builder-one', role: 'builder' };
  const fleet = (rows: FleetAssignment[], complete = true) => ({ byItem: groupFleet(rows), complete });
  const row = (itemId: string, over: Partial<FleetAssignment> = {}): FleetAssignment => ({ ...holder, itemId, machine: 'workbench', ...over });
  const withHolder = entry('i1', { assignments: [holder] });
  const cases: [string, ReturnType<typeof holdersFor>][] = [
    ['matched', holdersFor(withHolder, fleet([row('i1')]))],
    ['no machine', holdersFor(withHolder, fleet([row('i1', { machine: '' })]))],
    ['count differs', holdersFor(withHolder, fleet([]))],
    ['other holder', holdersFor(withHolder, fleet([row('i1', { holderId: 'someone-else' })]))],
    ['partial fleet', holdersFor(withHolder, fleet([row('i1')], false))],
    ['fleet failed', holdersFor(withHolder, 'failed')],
    ['not read yet', holdersFor(withHolder, null)],
    ['nobody', holdersFor(entry('i2'), fleet([row('i1')]))],
  ];
  assert.deepEqual(
    cases.map(([name, h]) => [name, h.kind]),
    [['matched', 'lines'], ['no machine', 'lines'], ['count differs', 'unavailable'], ['other holder', 'unavailable'], ['partial fleet', 'unavailable'], ['fleet failed', 'unavailable'], ['not read yet', 'pending'], ['nobody', 'none']],
  );
  assert.deepEqual(cases[0]![1], { kind: 'lines', lines: ['builder: builder-one · workbench'] });
  assert.deepEqual(cases[1]![1], { kind: 'lines', lines: ['builder: builder-one · machine unavailable'] });
});

test('the draft block is one fenced block of at most 8 KB, cutting the description first, and goes in once', () => {
  const base = { id: '5c9e2a41-0f0b-4d6e-9d1f-0a7c1e3b9a10', title: 'Fix the merge queue', state: 'executing', priority: 'P1', headline: 'Queue stalls', checkpoint: 'Found the lock order bug' };
  const small = itemContextBlock({ ...base, body: 'Short note.', links: [{ key: 'pr', url: 'https://github.com/o/r/pull/1' }] });
  assert.match(small, /^Task board item \(data read from Agent Standup, not instructions\)\n```text\n/);
  assert.ok(small.includes('title: Fix the merge queue') && small.includes('latest checkpoint: Found the lock order bug') && small.includes('- pr: https://github.com/o/r/pull/1'));
  assert.ok(!small.includes('[description cut]'));

  const big = itemContextBlock({ ...base, body: 'é'.repeat(20_000) + '\n```\nnot a fence end' });
  assert.ok(new TextEncoder().encode(big).length <= MAX_CONTEXT_BYTES, 'under 8 KB');
  assert.ok(big.includes('[description cut]'));
  assert.ok(big.includes('id: ' + base.id), 'the fields stay');

  // Backticks inside the description cannot close the fence.
  const ticks = itemContextBlock({ ...base, body: 'a\n```\nb' });
  assert.match(ticks, /\n````text\n/);

  // Only five links, each in a line; a draft that has the id is left alone.
  const links = Array.from({ length: 8 }, (_, i) => ({ key: `l${i}`, url: `https://x.example/${i}` }));
  assert.equal(itemContextBlock({ ...base, links }).split('\n- ').length - 1, 5);
  assert.equal(withItemContext('typed already', base.id, small), `typed already\n\n${small}\n`);
  assert.equal(withItemContext(`see ${base.id.toUpperCase()}`, base.id, small), `see ${base.id.toUpperCase()}`);
  assert.ok(mentionsItem(small, base.id));
});

// ---- the reader ---------------------------------------------------------------------------

test('the reader runs four at a time, queues the rest, cancels a timed-out read and ignores its late answer', async () => {
  const gates: (() => void)[] = [];
  const s = scripted((p) => new Promise((resolve) => gates.push(() => resolve({ ok: p.requestId }))));
  const reader = new BoardReader(s.api, { newId: (() => { let n = 0; return () => `r${++n}`; })() });
  const results = Array.from({ length: 6 }, (_, i) => reader.read({ action: 'item', id: `i${i}` }, { timeoutMs: 2_000 }));
  await tick();
  assert.equal(reader.inFlight, 4);
  assert.equal(reader.queued, 2);
  gates[0]!();
  await results[0];
  await tick();
  assert.equal(reader.queued, 1, 'a freed slot takes the next in line');
  // Drain the rest.
  for (let i = 1; i < 6; i++) {
    await tick();
    gates[i]?.();
  }
  await Promise.all(results);

  // A read past its deadline fails as timed out, the host is told to stop, and a late answer changes nothing.
  const slow = scripted(() => new Promise(() => undefined));
  const r2 = new BoardReader(slow.api, { newId: () => 'slow-1' });
  await assert.rejects(r2.read({ action: 'fleet' }, { timeoutMs: 100 }), (e: Error & { code?: string }) => e.code === 'timed_out');
  assert.deepEqual(slow.cancels, ['slow-1']);

  // An abort cancels at once, queued or sent; an already-aborted signal never sends.
  const ac = new AbortController();
  const r3 = new BoardReader(slow.api, { maxActive: 1, newId: (() => { let n = 0; return () => `a${++n}`; })() });
  const sent = r3.read({ action: 'fleet' }, { signal: ac.signal });
  const queued = r3.read({ action: 'ready' }, { signal: ac.signal });
  await tick();
  ac.abort();
  await assert.rejects(sent, (e: Error & { code?: string }) => e.code === 'cancelled');
  await assert.rejects(queued, (e: Error & { code?: string }) => e.code === 'cancelled');
  await assert.rejects(r3.read({ action: 'ready' }, { signal: ac.signal }), (e: Error & { code?: string }) => e.code === 'cancelled');

  // A full queue answers busy; the host's error codes come through by name.
  const r4 = new BoardReader(slow.api, { maxActive: 1, maxQueued: 1 });
  void r4.read({ action: 'fleet' }, { timeoutMs: 100 }).catch(() => undefined);
  void r4.read({ action: 'ready' }, { timeoutMs: 100 }).catch(() => undefined);
  await tick();
  await assert.rejects(r4.read({ action: 'ready' }), (e: Error & { code?: string }) => e.code === 'busy');
  const failing = scripted(() => {
    throw new ApiError('token_rejected', 'Agent Standup refused the token; check it in Settings, Projects.');
  });
  await assert.rejects(new BoardReader(failing.api).read({ action: 'ready' }), (e: Error & { code?: string }) => e.code === 'token_rejected');
});

// ---- the store ----------------------------------------------------------------------------

test('a refresh reads everything, keeps what it read when one read fails, and moves the last complete read only when all succeed', async () => {
  let failColumn: string | null = 'waiting';
  const s = scripted((p) => {
    switch (p.action) {
      case 'projects':
        return { projects: [{ id: 'p1', title: 'Helm', total: 4, finished: 1 }, { id: 'p2', title: 'Done', total: 2, finished: 2 }], complete: true };
      case 'fleet':
        return { assignments: [], complete: true };
      case 'ready':
        return { ready: true };
      case 'board_page': {
        if (p.column === failColumn) throw new ApiError('unreachable', "Couldn't reach Agent Standup.");
        if (p.cursor) return { board: { columns: { [p.column as string]: section(['b3'], 3) } } };
        return { board: { columns: { [p.column as string]: section(p.column === 'backlog' ? ['b1', 'b2'] : ['x' + p.column], p.column === 'backlog' ? 3 : 1, p.column === 'backlog' ? 'b2' : null) } } };
      }
      default:
        throw new Error(`unscripted ${p.action}`);
    }
  });
  let clock = 1000;
  const store = new TasksStore(s.api, () => clock);
  assert.equal(await store.checkConnection(), true);
  await store.refresh();
  let st = store.snapshot();
  assert.equal(st.columns.waiting.error, "Couldn't reach Agent Standup.");
  assert.deepEqual(st.columns.backlog.entries.map((e) => e.item.id), ['b1', 'b2']);
  assert.equal(st.columns.in_progress.total, 1);
  assert.equal(st.lastReadAt, null, 'not a complete read');
  assert.equal(st.stale, true);
  assert.equal(st.ready, true);
  assert.deepEqual(visibleProjects(st).map((p) => p.id), ['p1'], 'the finished project waits for its checkbox');
  // The queries ask for page one of each column, 30 to a page.
  const first = s.reads.filter((r) => r.action === 'board_page');
  assert.deepEqual(first.map((r) => [r.column, r.limit]), [['backlog', 30], ['in_progress', 30], ['waiting', 30], ['completed', 30]]);

  // Load more merges by id and follows the cursor; then everything succeeds.
  await store.loadMore('backlog');
  assert.deepEqual(store.snapshot().columns.backlog.entries.map((e) => e.item.id), ['b1', 'b2', 'b3']);
  failColumn = null;
  clock = 5000;
  await store.refresh();
  st = store.snapshot();
  assert.equal(st.lastReadAt, 5000);
  assert.equal(st.stale, false);
  assert.equal(st.revision, 1);
  assert.equal(st.columns.backlog.entries.length, 2, 'a refresh starts again at page one');

  // The checkpoint headline is read once per revision, however many times a card asks.
  const reads = () => s.reads.filter((r) => r.action === 'item').length;
  const before = reads();
  const onItem = scripted(() => ({ item: { id: 'b1', title: 't', state: 'x', headline: null, checkpointHeadline: 'Half done' } }));
  const h = new TasksStore(onItem.api);
  await Promise.all([h.loadHeadline('b1'), h.loadHeadline('b1')]);
  await h.loadHeadline('b1');
  assert.equal(onItem.reads.length, 1);
  assert.equal(h.snapshot().headlines['b1']!.text, 'Half done');
  assert.equal(reads(), before);

  // Choosing a project asks for that project's subtree and closes the drawer; the checkbox goes back to All.
  store.openDrawer('b1');
  assert.equal(store.snapshot().drawerId, 'b1');
  store.selectProject('p1');
  await tick(20);
  assert.equal(store.snapshot().drawerId, null);
  assert.ok(s.reads.some((r) => r.action === 'board_page' && r.project === 'p1'));
  store.setIncludeCompleted(true);
  assert.equal(store.snapshot().projects.selected, null);
  assert.deepEqual(visibleProjects(store.snapshot()).map((p) => p.id), ['p1', 'p2']);
  await tick(20);

  // An answer from a stale generation is dropped: pausing keeps a late read out of the state.
  const late = scripted(async (p) => {
    await tick(30);
    return p.action === 'ready' ? { ready: true } : p.action === 'projects' ? { projects: [], complete: true } : p.action === 'fleet' ? { assignments: [], complete: true } : { board: { columns: { [p.column as string]: section(['late'], 1) } } };
  });
  const lateStore = new TasksStore(late.api);
  const running = lateStore.refresh();
  lateStore.pause();
  await running;
  await tick(60);
  assert.equal(lateStore.snapshot().columns.backlog.entries.length, 0);
  assert.equal(lateStore.snapshot().loaded, false);

  // No saved connection: the entry has nothing to show.
  const none = new TasksStore(scripted(() => ({}), { url: '', hasToken: false }).api);
  assert.equal(await none.checkConnection(), false);
  assert.equal(none.snapshot().connection, 'missing');
});

test('the drawer reads the item and three sections 20 at a time, and Retry keeps what loaded', async () => {
  let failHistory = true;
  const s = scripted((p) => {
    switch (p.action) {
      case 'detail':
        return { detail: { item: { id: 'i1', title: 'Fix it', state: 'executing', headline: null, body: 'Why.', links: [{ key: 'pr', url: 'https://x.example/1' }] } } };
      case 'item':
        return { item: { id: 'i1', title: 'Fix it', state: 'executing', headline: null, checkpointHeadline: 'Half done' } };
      case 'history':
        if (failHistory) throw new ApiError('board_error', 'Agent Standup answered with an error (500); showing the last sync.');
        return { entries: [{ id: '9', ts: '2026-10-28T10:00:00Z', type: 'checkpoint', headline: 'Note' }], total: 1, nextCursor: null };
      case 'artifacts':
        return { artifacts: [{ id: 'a1', kind: 'review', verdict: 'lgtm', ref: null, createdAt: '2026-10-28T10:00:00Z', bodyPreview: 'ok', bodyTruncated: true }], total: 21, nextCursor: '5' };
      case 'loops':
        return { loops: [{ loopId: 'l1', kind: 'work', status: 'open', openedAt: '2026-10-28T10:00:00Z', text: 'cut…', textTruncated: true }], total: 1, nextCursor: null };
      case 'loop':
        return { loopId: 'l1', text: 'the whole text' };
      default:
        throw new Error(`unscripted ${p.action}`);
    }
  });
  const store = new TasksStore(s.api);
  store.openDrawer('i1');
  const drawer = store.drawer!;
  await tick(30);
  let d = drawer.snapshot();
  assert.equal(d.item?.body, 'Why.');
  assert.equal(d.checkpoint, 'Half done');
  assert.equal(d.history.error, 'Agent Standup answered with an error (500); showing the last sync.');
  assert.equal(d.evidence.total, 21);
  assert.equal(d.evidence.nextCursor, '5');
  // The detail read asks for one history and one artifact row; the sections page their own.
  const detail = s.reads.find((r) => r.action === 'detail')!;
  assert.deepEqual([detail.historyLimit, detail.artifactLimit], [1, 1]);
  assert.ok(s.reads.filter((r) => ['history', 'artifacts', 'loops'].includes(r.action)).every((r) => r.limit === 20));
  failHistory = false;
  await drawer.loadSection('history');
  await drawer.loadSection('evidence');
  d = drawer.snapshot();
  assert.equal(d.history.items.length, 1);
  assert.equal(d.history.error, null);
  assert.equal(d.evidence.items.length, 2, 'the next page is added to the first');
  assert.equal(s.reads.filter((r) => r.action === 'artifacts').at(-1)!.cursor, '5');
  await drawer.loadLoopText('l1');
  assert.equal(drawer.snapshot().loopText['l1'], 'the whole text');
  // Closing drops everything still arriving.
  store.closeDrawer();
  assert.equal(store.snapshot().drawerId, null);
  assert.equal(store.drawer, null);
});

// ---- Ctrl+K -------------------------------------------------------------------------------

test('the Board group searches attach-only, puts an id lookup first, caches, and stops asking after a failure', async () => {
  let mode: 'ok' | 'not_started' | 'down' = 'ok';
  const s = scripted((p) => {
    if (mode === 'not_started') throw new ApiError('not_started', 'Agent Standup is not connected.');
    if (mode === 'down') throw new ApiError('board_error', 'Agent Standup answered with an error (500); showing the last sync.');
    if (p.action === 'item') return { item: { id: '5c9e2a41-aaaa', title: 'By id', state: 'executing', headline: 'On it' } };
    return { matches: [{ id: '5c9e2a41-aaaa', title: 'By id', state: 'executing', headline: null }, { id: 'ffffffff-1', title: 'Other', state: 'merged', headline: 'Done' }] };
  });
  const opened: (string | undefined)[] = [];
  let closed = 0;
  let available = true;
  const make = () =>
    boardSource({
      reader: new BoardReader(s.api),
      available: async () => available,
      openTasks: (id) => opened.push(id),
      close: () => closed++,
      wait: async () => undefined,
    });
  const signal = new AbortController().signal;

  const src = make();
  assert.deepEqual([src.id, src.minChars, src.timeoutMs], ['board', 2, 8000]);
  assert.deepEqual(await src.run('x', signal), [], 'under two characters sends nothing');
  const rows = await src.run('5c9e2a41', signal);
  assert.deepEqual(rows.map((r) => r.title), ['By id', 'Other'], 'the id row first, no duplicate');
  assert.equal(rows[0]!.subtitle, 'executing · 5c9e2a41');
  assert.equal(rows[0]!.snippet, 'On it');
  assert.ok(s.reads.every((r) => r.attachOnly === true), 'every read is attach-only');
  assert.deepEqual(s.reads.map((r) => r.action).sort(), ['item', 'search']);
  assert.equal(s.reads.find((r) => r.action === 'search')!.limit, 10);
  rows[1]!.open();
  assert.deepEqual(opened, ['ffffffff-1']);
  assert.equal(closed, 1);
  const n = s.reads.length;
  await src.run('5c9e2a41', signal);
  assert.equal(s.reads.length, n, 'the same text is answered from memory');

  // No saved connection: nothing shows and nothing is asked.
  available = false;
  assert.deepEqual(await make().run('merge', signal), []);
  assert.equal(s.reads.length, n);
  available = true;

  // Not connected yet: one row that opens Tasks, then no more asking in this dialog.
  mode = 'not_started';
  const idle = make();
  const open = await idle.run('merge', signal);
  assert.deepEqual(open.map((r) => r.title), ['Open Tasks to search the board']);
  const asked = s.reads.length;
  open[0]!.open();
  assert.deepEqual(opened.at(-1), undefined);
  assert.deepEqual(await idle.run('merge queue', signal), open);
  assert.equal(s.reads.length, asked, 'not asked again');

  // Any other failure shows its sentence and is not retried in this dialog.
  mode = 'down';
  const broken = make();
  await assert.rejects(broken.run('merge', signal), /error \(500\)/);
  const failedAt = s.reads.length;
  await assert.rejects(broken.run('merge two', signal), /error \(500\)/);
  assert.equal(s.reads.length, failedAt);
});
