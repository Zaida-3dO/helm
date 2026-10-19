import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { buildList, chatAncestors, HEIGHTS, nestRows, type ListItem } from './model.ts';
import { loadMore, readBudget, type PageResult } from './paging.ts';
import { age, rowAttention, trailingIndicator } from './status.ts';
import { layout, visibleRange } from './virtual.ts';

function project(id: string, over: Partial<ProjectNode> = {}): ProjectNode {
  return {
    id,
    kind: 'folder',
    name: id,
    path: `C:/w/${id}`,
    parentId: null,
    parentSource: null,
    icon: null,
    colour: null,
    pinned: false,
    collapsed: false,
    position: null,
    archived: false,
    board: null,
    primaryFolderProjectId: null,
    chatCount: 0,
    createdAt: 1,
    revision: 0,
    ...over,
  };
}

function row(chatId: string, group: string, over: Partial<SidebarRow> = {}): SidebarRow {
  return {
    chatId,
    title: chatId,
    groupProjectId: group,
    folderProjectId: group,
    folderPath: `C:/w/${group}`,
    depth: 0,
    parentChatId: null,
    hasChildren: false,
    status: 'idle',
    attention: null,
    pinned: false,
    unread: false,
    muted: false,
    stalled: false,
    lastActivityAt: 1,
    archived: false,
    position: null,
    createdAt: 1,
    updatedAt: 1,
    revision: 1,
    ...over,
  };
}

const opts = { view: 'projects' as const, compact: false, expandedChats: new Set<string>(), selectedChatId: null };
const shape = (items: ListItem[]) =>
  items.map((i) =>
    i.kind === 'header' ? `H${i.depth}:${i.project.id}` : i.kind === 'chat' ? `C${i.depth}:${i.row.chatId}` : i.kind === 'emptyToggle' ? `E${i.count}${i.shown ? '+' : '-'}` : '--',
  );

test('status priority: running > unread > lingering > interrupted > pinned', () => {
  const base = { status: 'idle' as const, unread: false, lingering: false, pinned: false };
  assert.equal(rowAttention({ ...base, status: 'stopping', unread: true, lingering: true, pinned: true }), 'running');
  assert.equal(rowAttention({ ...base, status: 'interrupted', unread: true, lingering: true, pinned: true }), 'unread');
  assert.equal(rowAttention({ ...base, status: 'interrupted', lingering: true, pinned: true }), 'lingering');
  assert.equal(rowAttention({ ...base, status: 'interrupted', pinned: true }), 'interrupted');
  assert.equal(rowAttention({ ...base, pinned: true }), 'pinned');
  assert.equal(rowAttention(base), null);
  assert.equal(trailingIndicator('lingering'), true);
  assert.equal(trailingIndicator('interrupted'), false);
});

test('age is relative: now, minutes, hours, days', () => {
  const t = 1_000_000_000;
  assert.equal(age(t - 59_000, t), 'now');
  assert.equal(age(t - 5 * 60_000, t), '5m');
  assert.equal(age(t - 3 * 3_600_000, t), '3h');
  assert.equal(age(t - 2 * 86_400_000, t), '2d');
});

test('a three-level project tree renders depths 0/1/2 with child projects before chats', () => {
  const projects = [
    project('a', { kind: 'board', path: null }),
    project('b', { parentId: 'a' }),
    project('c', { parentId: 'b' }),
  ];
  const rows = [row('x', 'a'), row('y', 'c')];
  assert.deepEqual(shape(buildList(projects, rows, opts)), ['H0:a', 'H1:b', 'H2:c', 'C0:y', 'C0:x']);
});

test('a missing parent draws at the top level and a collapsed group hides its contents', () => {
  const projects = [project('a', { parentId: 'gone', chatCount: 1 }), project('b', { collapsed: true }), project('c', { parentId: 'b', chatCount: 1 })];
  const rows = [row('x', 'b')];
  assert.deepEqual(shape(buildList(projects, rows, opts)), ['H0:a', 'H0:b']);
  // The open chat's group stays expanded.
  assert.deepEqual(shape(buildList(projects, rows, { ...opts, selectedChatId: 'x' })), ['H0:a', 'H0:b', 'H1:c', 'C0:x']);
});

test('empty projects hide by default, with a "Show N empty projects" row that brings them back', () => {
  const projects = [
    project('full', { kind: 'board', path: null, chatCount: 2 }), // counted by the host, no rows held yet
    project('held'), // count still 0, but a held row stands in
    project('bare', { kind: 'board', path: null }),
    project('parent'), // empty itself, but its child has chats
    project('kid', { parentId: 'parent', chatCount: 1 }),
    project('shell'), // empty, with an empty child: two hidden
    project('shellKid', { parentId: 'shell' }),
    project('pinned', { pinned: true }),
    project('open'),
    project('gone', { kind: 'board', path: null, board: { stale: true } as ProjectNode['board'] }),
    project('arch', { archived: true }),
  ];
  const rows = [row('h1', 'held'), row('o1', 'open', { archived: true })];
  const hidden = buildList(projects, rows, opts);
  assert.deepEqual(shape(hidden), ['H0:pinned', '--', 'H0:full', 'H0:held', 'C0:h1', 'H0:parent', 'H1:kid', 'E4-']);
  // The open chat's group shows even when it holds nothing else (here its chat is archived).
  const open = buildList(projects, rows, { ...opts, selectedChatId: 'o1' });
  assert.ok(shape(open).includes('H0:open'));
  assert.deepEqual(shape(open).slice(-1), ['E3-']);
  // Shown: every empty group draws, in host order; stale and archived groups stay hidden.
  const shown = buildList(projects, rows, { ...opts, showEmpty: true });
  assert.deepEqual(shape(shown), [
    'H0:pinned',
    '--',
    'H0:full',
    'H0:held',
    'C0:h1',
    'H0:bare',
    'H0:parent',
    'H1:kid',
    'H0:shell',
    'H1:shellKid',
    'H0:open',
    'E4+',
  ]);
  // No toggle row when nothing is empty, nor in the activity or archived views.
  assert.deepEqual(shape(buildList([project('full', { chatCount: 1 })], [], opts)), ['H0:full']);
  assert.ok(!shape(buildList(projects, rows, { ...opts, view: 'archived' })).some((s) => s.startsWith('E')));
  assert.ok(!shape(buildList(projects, rows, { ...opts, view: 'activity' })).some((s) => s.startsWith('E')));
});

test('a pin break separates pinned and unpinned top-level groups', () => {
  const items = buildList([project('p', { pinned: true }), project('q', { chatCount: 3 })], [], opts);
  assert.deepEqual(shape(items), ['H0:p', '--', 'H0:q']);
  assert.equal(items[1]!.height, HEIGHTS.normal.pinBreak);
});

test('chats nest under held parents in the same group, capped at depth 2, collapsed by default', () => {
  const rows = [
    row('a', 'g'),
    row('b', 'g', { parentChatId: 'a' }),
    row('c', 'g', { parentChatId: 'b' }),
    row('d', 'g', { parentChatId: 'c' }), // a deeper record renders at depth 2
    row('e', 'g', { parentChatId: 'nope' }), // parent not held: top level
    row('f', 'other', { parentChatId: 'a' }), // parent in another group: top level there
  ];
  const all = new Set(['a', 'b', 'c']);
  const items = buildList([project('g'), project('other')], rows, { ...opts, expandedChats: all });
  assert.deepEqual(shape(items), ['H0:g', 'C0:a', 'C1:b', 'C2:c', 'C2:d', 'C0:e', 'H0:other', 'C0:f']);
  // Collapsing the parent hides every descendant.
  const collapsed = buildList([project('g')], rows.slice(0, 3), opts);
  assert.deepEqual(shape(collapsed), ['H0:g', 'C0:a']);
  const a = collapsed[1] as Extract<ListItem, { kind: 'chat' }>;
  assert.equal(a.children, 1);
  assert.equal(a.expanded, false);
});

test('a chat cycle is cut rather than looping', () => {
  const { order } = nestRows([row('a', 'g', { parentChatId: 'b' }), row('b', 'g', { parentChatId: 'a' })]);
  assert.deepEqual(
    order.map((o) => [o.row.chatId, o.depth]),
    [
      ['a', 0],
      ['b', 0],
    ],
  );
});

test('activity is flat, newest first, two-line; archived shows only groups holding archived chats', () => {
  const rows = [row('old', 'g', { updatedAt: 1 }), row('new', 'h', { updatedAt: 9 }), row('arc', 'h', { archived: true })];
  const act = buildList([project('g'), project('h')], rows, { ...opts, view: 'activity' });
  assert.deepEqual(shape(act), ['C0:new', 'C0:old']);
  assert.equal(act[0]!.height, HEIGHTS.normal.twoLine);
  const arc = buildList([project('g'), project('h')], rows, { ...opts, view: 'archived' });
  assert.deepEqual(shape(arc), ['H0:h', 'C0:arc']);
});

test('row heights match the design system, normal and compact', () => {
  const n = buildList([project('g')], [row('a', 'g')], opts);
  assert.deepEqual(n.map((i) => i.height), [40, 36]);
  const c = buildList([project('g')], [row('a', 'g')], { ...opts, compact: true });
  assert.deepEqual(c.map((i) => i.height), [34, 28]);
  const a = buildList([project('g')], [row('a', 'g')], { ...opts, compact: true, view: 'activity' });
  assert.deepEqual(a.map((i) => i.height), [38]);
});

test('a stale board group shows only while it still holds chats', () => {
  const stale = project('s', { kind: 'board', path: null, board: { boardProjectId: 'x', url: 'u', title: 's', headline: null, area: null, repo: null, priority: null, lastActivity: null, stale: true, editedLocally: false } });
  assert.deepEqual(shape(buildList([stale], [], opts)), []);
  assert.deepEqual(shape(buildList([stale], [row('a', 's')], opts)), ['H0:s', 'C0:a']);
});

test('chatAncestors walks held parents', () => {
  const rows = [row('a', 'g'), row('b', 'g', { parentChatId: 'a' }), row('c', 'g', { parentChatId: 'b' })];
  assert.deepEqual(chatAncestors(rows, 'c'), ['b', 'a']);
});

test('virtual range mounts the viewport plus 8 rows of overscan', () => {
  const heights = new Array(1000).fill(36);
  const l = layout(heights);
  assert.equal(l.total, 36_000);
  const [s, e] = visibleRange(l, heights, 3600, 360);
  assert.equal(s, 100 - 8);
  assert.equal(e, 110 + 8);
  assert.deepEqual(visibleRange(l, heights, 0, 360), [0, 18]);
  assert.ok(e - s < 50, 'fewer than 50 mounted rows');
});

// ---- paging stop rule ----

function pages(total: number, size = 500) {
  const ids = Array.from({ length: total }, (_, i) => `c${i}`);
  return async (cursor: string | null): Promise<PageResult<string>> => {
    const at = cursor ? Number(cursor) : 0;
    const rows = ids.slice(at, at + size);
    return { rows, nextCursor: at + size < total ? String(at + size) : null };
  };
}

test('one click stops at the first page that starts with a chat not held', async () => {
  const held = new Set(Array.from({ length: 1000 }, (_, i) => `c${i}`));
  let reads = 0;
  const fetch = pages(5000);
  const r = await loadMore<string>({
    cursor: null,
    held: (x) => held.has(x),
    heldCount: held.size,
    fetch: (c) => {
      reads++;
      return fetch(c);
    },
    merge: (rows) => rows.forEach((x) => held.add(x)),
  });
  assert.equal(reads, 3, 'two pages already held, then one new page');
  assert.equal(r.productive, 1);
  assert.equal(r.complete, false);
  assert.equal(held.size, 1500);
});

test('one click stops after 10 productive pages', async () => {
  // Every page starts with a held chat but brings new ones, so only the productive cap stops it.
  const held = new Set<string>(['seed']);
  let n = 0;
  const r = await loadMore<string>({
    cursor: null,
    held: (x) => held.has(x),
    heldCount: 0,
    fetch: async () => {
      n++;
      return { rows: ['seed', `n${n}`], nextCursor: String(n) };
    },
    merge: (rows) => rows.filter((x) => x !== 'seed').forEach((x) => held.add(x)),
  });
  assert.equal(r.productive, 10);
  assert.equal(r.reads, 10);
});

test('the read budget is 50 plus one per 500 held, so a click gets past 25,000 held chats', async () => {
  assert.equal(readBudget(0), 50);
  assert.equal(readBudget(25_000), 100);
  assert.equal(readBudget(25_001), 101);
  const total = 30_000;
  const held = new Set(Array.from({ length: 26_000 }, (_, i) => `c${i}`));
  const r = await loadMore<string>({
    cursor: null,
    held: (x) => held.has(x),
    heldCount: held.size,
    fetch: pages(total),
    merge: (rows) => rows.forEach((x) => held.add(x)),
  });
  assert.equal(r.reads, 53, '52 held pages walked, then the first new one');
  assert.equal(r.productive, 1);
  // A flat budget of 50 would have stopped inside the held pages with nothing new.
});

test('the list ending completes the view', async () => {
  const held = new Set<string>();
  const click = (cursor: string | null) =>
    loadMore<string>({
      cursor,
      held: (x) => held.has(x),
      heldCount: held.size,
      fetch: pages(700),
      merge: (rows) => rows.forEach((x) => held.add(x)),
    });
  const first = await click(null);
  assert.equal(first.complete, false, 'a fresh page of 500 ends the first click');
  const second = await click(first.cursor);
  assert.equal(second.complete, true);
  assert.equal(second.cursor, null);
  assert.equal(held.size, 700);
});
