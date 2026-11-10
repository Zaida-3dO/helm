import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode, ProjectOps, SidebarRow } from '../api/projects.ts';
import type { AttentionSummary } from '../api/types.ts';
import { SidebarData } from './data.ts';
import { toast } from '../ui/toasts.ts';
import { SidebarActions, type ActionDeps } from './actions.ts';
import { UndoStack } from './undo.ts';
import { buildList, chatAncestors, headerSubtitle, HEIGHTS, nestRows, showsCalmWheel, stepChat, visibleChatOrder, type ListItem } from './model.ts';
import { loadMore, readBudget, type PageResult } from './paging.ts';
import { age, needsYouLabel, rowAttention, rowStatusLabel, trailingIndicator } from './status.ts';
import { layout, visibleRange } from './virtual.ts';
import { mergeOrder, moveView, orderViews, readOrder, slotsFit, splitViews, touchView, writeOrder } from './tiles.ts';

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
    closed: false,
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
    local: false,
    localAhead: null,
    hasDraft: false,
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
    i.kind === 'header'
      ? `H${i.depth}:${i.project.id}`
      : i.kind === 'chat'
        ? `C${i.depth}:${i.row.chatId}`
        : '--',
  );

/** A board project; `completed` marks all its work finished on the board. */
function boardProject(id: string, completed: boolean, over: Partial<ProjectNode> = {}): ProjectNode {
  const board = { boardProjectId: id, url: 'https://board.example', title: id, headline: null, area: null, repo: null, priority: null, lastActivity: null, stale: false, completed, editedLocally: false };
  return project(id, { kind: 'board', path: null, ...over, board: { ...board, ...over.board } });
}

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
  // The flag on a chat waiting on an answer says so.
  assert.deepEqual(['question', 'plan', 'approval', 'triage', undefined].map((k) => needsYouLabel(k as never)), ['Answer question', 'Answer question', 'Answer question', 'Needs you', 'Needs you']);
  // A running local chat's wording speaks in halves of the wait at the model server (which
  // answers one request at a time); every other running chat reads as before.
  assert.equal(rowStatusLabel({ stalled: false, local: true, localAhead: 1 }), 'Waiting for the local model (1 ahead)');
  assert.equal(rowStatusLabel({ stalled: true, local: true, localAhead: 0 }), 'Waiting for the local model');
  assert.equal(rowStatusLabel({ stalled: false, local: true, localAhead: null }), 'Using the local model');
  assert.equal(rowStatusLabel({ stalled: true, local: false, localAhead: null }), 'Running, no output for a while');
  assert.equal(rowStatusLabel({ stalled: false, local: false, localAhead: null }), 'Task running');
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

test('a collapsed group totals the running and unread chats inside it and its nested projects', () => {
  const projects = [project('a', { collapsed: true, chatCount: 3 }), project('kid', { parentId: 'a', chatCount: 1 }), project('open', { chatCount: 1 })];
  const rows = [row('r1', 'a', { status: 'running' }), row('u1', 'a', { unread: true }), row('i1', 'a'), row('r2', 'kid', { status: 'starting' }), row('o1', 'open', { status: 'running' })];
  const header = (items: ListItem[], id: string) => items.find((i): i is Extract<ListItem, { kind: 'header' }> => i.kind === 'header' && i.project.id === id)!;
  const items = buildList(projects, rows, opts);
  assert.deepEqual(header(items, 'a').marks, { running: 2, unread: 1, flagged: 0, lingering: 0 });
  assert.equal(header(items, 'open').marks, null, 'an expanded group shows its own rows instead');
  assert.equal(header(buildList([project('b', { collapsed: true, chatCount: 1 })], [row('x', 'b')], opts), 'b').marks, null, 'nothing going on, no marks');
  // A waiting or flagged chat inside a collapsed group is counted on its header.
  assert.deepEqual(header(buildList(projects, rows, { ...opts, flagged: new Set(['i1', 'r2']) }), 'a').marks, { running: 2, unread: 1, flagged: 2, lingering: 0 });

  // Background work only: the header carries the calm wheel, unless a chat inside runs (running wins).
  const held = (extra: SidebarRow[]) => header(buildList([project('bg', { collapsed: true, chatCount: 3 })], [row('l1', 'bg', { attention: 'lingering' }), row('q1', 'bg'), ...extra], opts), 'bg').marks;
  const cases: [string, SidebarRow[], boolean][] = [
    ['background work only', [], true],
    ['plus a running chat', [row('r1', 'bg', { status: 'running', attention: 'running' })], false],
    ['plus an unread one', [row('u1', 'bg', { unread: true, attention: 'unread' })], true],
  ];
  for (const [what, extra, calm] of cases) assert.equal(showsCalmWheel(held(extra)!), calm, what);
  assert.equal(held([])?.lingering, 1);
  assert.equal(header(buildList([project('bg', { collapsed: true, chatCount: 1 })], [row('q1', 'bg')], opts), 'bg').marks, null, 'nothing going on: no mark');
});

test('chats in a closed project are never drawn, not even as loose rows, in any view', () => {
  const rows = [row('x', 'gone'), row('y', 'a'), row('z', 'gone', { archived: true })];
  const closed = new Set(['gone']);
  const ids = (items: ListItem[]) => items.filter((i) => i.kind === 'chat').map((i) => (i as Extract<ListItem, { kind: 'chat' }>).row.chatId);
  for (const view of ['projects', 'activity', 'archived'] as const) {
    const items = buildList([project('a', { chatCount: 1 })], rows, { ...opts, view, closed });
    assert.ok(!ids(items).some((id) => id === 'x' || id === 'z'), view);
  }
  assert.deepEqual(ids(buildList([project('a', { chatCount: 1 })], rows, opts)), ['y', 'x'], 'without the closed set they are loose rows');
});

test('the sidebar data drops a closed project rows, brings them back on reopen and reads flagged chats it does not hold', async () => {
  const closedNode = project('b', { closed: true });
  let listed: ProjectNode[] = [project('a'), project('b')];
  const read: string[][] = [];
  const stored = [row('x', 'b'), row('y', 'a'), row('f', 'a')];
  const data = new SidebarData({
    list: async () => listed,
    rows: async (ids: string[]) => {
      read.push(ids);
      return stored.filter((r) => ids.includes(r.chatId));
    },
    page: async () => ({ rows: stored.filter((r) => !listed.some((p) => p.closed && p.id === r.groupProjectId)), nextCursor: null }),
  } as unknown as ProjectOps);
  await data.loadProjects();
  await data.page('projects');
  assert.deepEqual(data.snapshot.rows.map((r) => r.chatId).sort(), ['f', 'x', 'y']);
  // Closing hides its chats at once, and a row event for one is ignored.
  listed = [project('a'), closedNode];
  data.project({ ...closedNode, revision: 1 });
  data.mergeRows([row('x', 'b', { revision: 2 })]);
  assert.deepEqual(data.snapshot.rows.map((r) => r.chatId).sort(), ['f', 'y']);
  assert.ok(data.snapshot.closed.has('b'));
  // Reopening starts every view again so the host's rows come back.
  listed = [project('a'), project('b')];
  data.project(project('b', { revision: 2 }));
  assert.ok(!data.snapshot.closed.has('b'));
  assert.equal(data.snapshot.paging.projects.started, false);
  await data.page('projects');
  assert.ok(data.snapshot.rows.some((r) => r.chatId === 'x'));
  // A chat waiting on you that the window has not loaded is read, once.
  const summary = (id: string) => ({ needsYou: [{ chatId: id, kind: 'approval' }], flagged: [] }) as unknown as AttentionSummary;
  data.attention(summary('unheld'));
  data.attention(summary('unheld'));
  assert.deepEqual(read, [['unheld']]);
});

test('a missing parent draws at the top level and a collapsed group hides its contents, even the open chat', () => {
  const projects = [project('a', { parentId: 'gone', chatCount: 1 }), project('b', { collapsed: true }), project('c', { parentId: 'b', chatCount: 1 })];
  const rows = [row('x', 'b', { status: 'running' }), row('y', 'c')];
  assert.deepEqual(shape(buildList(projects, rows, opts)), ['H0:a', 'H0:b']);
  // A chat just opened (from search, a link or a notification) shows its project and the projects it sits in.
  assert.deepEqual(shape(buildList(projects, rows, { ...opts, selectedChatId: 'x' })), ['H0:a', 'H0:b', 'H1:c', 'C0:y', 'C0:x']);
  const deep = [...projects.slice(0, 2), project('c', { parentId: 'b', chatCount: 1, collapsed: true })];
  assert.deepEqual(shape(buildList(deep, rows, { ...opts, selectedChatId: 'y' })), ['H0:a', 'H0:b', 'H1:c', 'C0:y', 'C0:x']);
  // The user's collapse wins over the open chat: its project stays shut and its header keeps the collapsed marks.
  const shut = buildList(projects, rows, { ...opts, selectedChatId: 'x', keptShut: new Set(['b']) });
  assert.deepEqual(shape(shut), ['H0:a', 'H0:b']);
  const head = shut[1] as Extract<ListItem, { kind: 'header' }>;
  assert.deepEqual([head.collapsed, head.marks], [true, { running: 1, unread: 0, flagged: 0, lingering: 0 }]);
  // Collapsing the inner project shuts only that one; the project it sits in stays open around it.
  assert.deepEqual(shape(buildList(deep, rows, { ...opts, selectedChatId: 'y', keptShut: new Set(['c']) })), ['H0:a', 'H0:b', 'H1:c', 'C0:x']);
});

test('empty projects never show, with no row to bring them back; a finished project shows while it holds chats', () => {
  const projects = [
    project('full', { kind: 'board', path: null, chatCount: 2 }), // counted by the host, no rows held yet
    project('held'), // count still 0, but a held row stands in
    project('bare', { kind: 'board', path: null }),
    project('parent'), // empty itself, but its child has chats
    project('kid', { parentId: 'parent', chatCount: 1 }),
    project('shell'), // empty, with an empty child: both hidden
    project('shellKid', { parentId: 'shell' }),
    project('pinned', { kind: 'board', path: null, pinned: true }),
    project('pinnedFolder', { pinned: true }), // a folder project is only where chats run: never shown just for a pin
    project('open', { kind: 'board', path: null }),
    project('openFolder'), // nor for holding the open chat's archived row
    project('gone', { kind: 'board', path: null, board: { stale: true } as ProjectNode['board'] }),
    project('arch', { archived: true }),
    boardProject('live', false, { chatCount: 1 }),
    boardProject('done', true, { chatCount: 2 }), // finished, with chats: stays in view, says so
    boardProject('doneRows', true), // finished, rows held though the count is 0
    boardProject('doneBare', true), // finished and empty: hidden
    boardProject('donePinned', true, { pinned: true }),
    boardProject('doneShell', true), // finished and empty, with a live child: drawn as its parent
    project('doneKid', { parentId: 'doneShell', chatCount: 1 }),
    boardProject('goneBare', true, { board: { stale: true } as ProjectNode['board'] }), // stale and empty
  ];
  const rows = [row('h1', 'held'), row('o1', 'open', { archived: true }), row('d1', 'doneRows')];
  const hidden = shape(buildList(projects, rows, opts));
  assert.deepEqual(hidden, ['H0:pinned', 'H0:donePinned', '--', 'H0:full', 'H0:held', 'C0:h1', 'H0:parent', 'H1:kid', 'H0:live', 'H0:done', 'H0:doneRows', 'C0:d1', 'H0:doneShell', 'H1:doneKid']);
  // A project's header and its chats are one block: first and last mark where the tint's corners round.
  assert.deepEqual(
    buildList(projects, rows, opts).map((i) => i.block ?? '-'),
    ['only', 'only', '-', 'only', 'first', 'last', 'only', 'only', 'only', 'only', 'first', 'last', 'only', 'only'],
  );
  assert.ok(buildList(projects, rows, { ...opts, view: 'activity' }).every((i) => i.block === undefined), 'activity rows stay separate');
  // The open chat's group shows even when it holds nothing else (here its chat is archived), if it is a board.
  assert.ok(shape(buildList(projects, rows, { ...opts, selectedChatId: 'o1' })).includes('H0:open'));
  assert.ok(!shape(buildList(projects, [...rows, row('o2', 'openFolder', { archived: true })], { ...opts, selectedChatId: 'o2' })).includes('H0:openFolder'));
  // A shown finished project says so under its name; a stale one still reads as gone.
  const shown = buildList(projects, rows, opts);
  const header = (id: string) => shown.find((i): i is Extract<ListItem, { kind: 'header' }> => i.kind === 'header' && i.project.id === id)!;
  assert.equal(header('done').subtitle, 'Finished');
  assert.equal(header('done').height, HEIGHTS.normal.headerSubtitle);
  assert.equal(header('live').subtitle, null);
  // A board with no folder offers "Set folder"; one with a folder, a folder project and a stale board do not.
  assert.deepEqual(['live', 'held', 'pinned'].map((id) => header(id).needsFolder), [true, false, true]);
  const withFolder = buildList([boardProject('b', false, { chatCount: 1, primaryFolderProjectId: 'f' }), project('f')], [], opts);
  assert.equal((withFolder[0] as Extract<ListItem, { kind: 'header' }>).needsFolder, false);
  const archivedFolder = buildList([boardProject('b', false, { chatCount: 1, primaryFolderProjectId: 'f' }), project('f', { archived: true })], [], opts);
  assert.equal((archivedFolder[0] as Extract<ListItem, { kind: 'header' }>).needsFolder, true, 'an archived folder is no folder');
  assert.equal(headerSubtitle(project('gone', { board: { stale: true } as ProjectNode['board'] }), new Map()), 'Not on the board');
  assert.equal(headerSubtitle(project('f', { board: null }), new Map()), null);
  // The archived view keeps its own rule: a finished project holding an archived chat shows there.
  assert.deepEqual(shape(buildList(projects, [row('a1', 'done', { archived: true })], { ...opts, view: 'archived' })), ['H0:arch', 'H0:done', 'C0:a1']);
  // An older board sends no counts, so nothing is finished.
  assert.deepEqual(shape(buildList([boardProject('x', false, { chatCount: 1 })], [], opts)), ['H0:x']);
});

test('the drawn chat order skips collapsed groups and parents; Ctrl+Tab steps through it, wrapping', () => {
  const projects = [project('g', { chatCount: 3 }), project('shut', { collapsed: true, chatCount: 1 })];
  const rows = [row('a', 'g'), row('b', 'g', { parentChatId: 'a' }), row('c', 'g'), row('x', 'shut')];
  const order = visibleChatOrder(buildList(projects, rows, opts));
  assert.deepEqual(order, ['a', 'c']); // b sits under collapsed a; x is in a collapsed group
  assert.equal(stepChat(order, 'a', 1), 'c');
  assert.equal(stepChat(order, 'c', 1), 'a');
  assert.equal(stepChat(order, 'a', -1), 'c');
  assert.equal(stepChat(order, 'b', 1), 'a'); // not drawn: next is the first
  assert.equal(stepChat(order, null, -1), 'c'); // previous is the last
  assert.equal(stepChat([], 'a', 1), null);
});

test('a pin break separates pinned and unpinned top-level groups', () => {
  const items = buildList([project('p', { kind: 'board', path: null, pinned: true }), project('q', { chatCount: 3 })], [], opts);
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

test('chats in a group follow the host order: pinned first, then dragged position, then newest', () => {
  const rows = [
    row('a', 'g', { createdAt: 1 }),
    row('b', 'g', { createdAt: 2 }),
    row('c', 'g', { createdAt: 3 }),
    row('d', 'g', { createdAt: 4, pinned: true }),
    row('e', 'g', { createdAt: 0, position: 0 }),
  ];
  assert.deepEqual(shape(buildList([project('g')], rows, opts)), ['H0:g', 'C0:d', 'C0:e', 'C0:c', 'C0:b', 'C0:a']);
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
  const stale = project('s', { kind: 'board', path: null, board: { boardProjectId: 'x', url: 'u', title: 's', headline: null, area: null, repo: null, priority: null, lastActivity: null, stale: true, completed: false, editedLocally: false } });
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

test('dialog saves wait for the host and answer with why they failed', async () => {
  const seen: unknown[] = [];
  const fail = new Error('The host refused');
  const mk = (api: () => Promise<unknown>, update: () => Promise<ProjectNode>) =>
    new SidebarActions({
      api,
      pops: { update },
      data: { project: (p: ProjectNode) => seen.push(p.id), loadProjects: async () => void seen.push('reload') },
    } as unknown as ActionDeps);
  const row = { chatId: 'c1' } as SidebarRow;
  const ok = mk(async () => null, async () => project('p1'));
  assert.equal(await ok.renameChat(row, 'New'), null);
  assert.equal(await ok.editProject(project('p1'), { name: 'x' }), null);
  assert.deepEqual(seen, ['p1']);
  const bad = mk(async () => { throw fail; }, async () => { throw fail; });
  assert.equal(await bad.renameChat(row, 'New'), 'The host refused');
  assert.equal(await bad.editProject(project('p1'), { name: 'x' }), 'The host refused');
  assert.deepEqual(seen, ['p1', 'reload']);
});

test('archiving a chat or a project shows an Undo toast whose button undoes it', async () => {
  const log: string[] = [];
  const act = new SidebarActions({
    undo: new UndoStack(),
    onError: (m: string) => log.push(`error ${m}`),
    ops: { stop: async (id: string) => void log.push(`stop ${id}`), archive: async (id: string) => void log.push(`archive ${id}`), restore: async (id: string) => void log.push(`restore ${id}`) },
    pops: { archive: async (id: string) => void log.push(`archive-project ${id}`), restore: async (id: string) => void log.push(`restore-project ${id}`) },
    data: { refetch: async () => undefined, loadProjects: async () => undefined, invalidatePaging: () => undefined },
  } as unknown as ActionDeps);
  // A running chat is archived at once: one archive call, no stop and no retry from here (the host stops it).
  await act.archive({ chatId: 'c1', title: 'Plans', status: 'running' } as SidebarRow);
  assert.equal(toast.current()?.kind, 'undo');
  assert.equal(toast.current()?.text, 'Archived “Plans”');
  // The button undoes the archive it announced, even when another archive came after it.
  const plans = toast.current()!;
  await act.archive({ chatId: 'c2', title: 'Other' } as SidebarRow);
  plans.action!.run();
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(log, ['archive c1', 'archive c2', 'restore c1']);
  await act.undo();
  assert.deepEqual(log.slice(3), ['restore c2']);
  await act.archiveProject(project('p1', { name: 'Atlas' }));
  assert.equal(toast.current()?.text, 'Archived “Atlas”');
  toast.dismiss(toast.current()!.id);
});

test('dropping a chat before another sends the target row level in one call, with an undo back to where it was', async () => {
  const calls: unknown[] = [];
  const undo = new UndoStack();
  let held = [row('a', 'g', { createdAt: 1 }), row('c', 'g', { createdAt: 3 }), row('e', 'g', { createdAt: 2 }), row('b', 'g', { parentChatId: 'p', createdAt: 5 }), row('b2', 'g', { parentChatId: 'p', createdAt: 4 })];
  // g and h are boards; f is a folder group, which a chat can leave but never be moved back to.
  const projects = [boardProject('g', false), boardProject('h', false), project('f')];
  const act = new SidebarActions({
    undo,
    onError: (m: string) => calls.push(`error ${m}`),
    pops: { organize: async (a: unknown) => (calls.push(a), []) },
    // Top level of g, newest first: c, a (so a is followed by nothing); b is the only child of p.
    data: { mergeRows: () => undefined, getSnapshot: () => ({ rows: held, projects }) },
  } as unknown as ActionDeps);
  // Out of a folder group onto a board, by a drop or by Move to project: the move happens, with no undo entry.
  await act.reorderBefore(row('x', 'f'), 'z', { parentChatId: null, pinned: false, groupProjectId: 'h' });
  await act.moveTo(row('y', 'f'), 'h');
  assert.deepEqual(calls.splice(0), [
    { chatId: 'x', parentChatId: null, pinned: false, beforeChatId: 'z', groupProjectId: 'h' },
    { chatId: 'y', groupProjectId: 'h', parentChatId: null },
  ]);
  assert.equal(undo.size, 0);
  // Before a nested child of p, from the top level: it takes parent p.
  await act.reorderBefore(row('a', 'g'), 'k', { parentChatId: 'p', pinned: false });
  // Before a top-level row of another group, from under p: top level there.
  await act.reorderBefore(row('b', 'g', { parentChatId: 'p' }), 'z', { parentChatId: null, pinned: false, groupProjectId: 'h' });
  // Same level: nothing to undo.
  await act.reorderBefore(row('c', 'g'), 'k', { parentChatId: null, pinned: false });
  // e sat before a, then a is pinned (leaves that level): e's undo goes last rather than being refused.
  await act.reorderBefore(row('e', 'g', { createdAt: 2 }), 'k', { parentChatId: 'p', pinned: false });
  held = held.map((r) => (r.chatId === 'a' ? { ...r, pinned: true } : r));
  assert.deepEqual(calls.slice(0, 3), [
    { chatId: 'a', parentChatId: 'p', pinned: false, beforeChatId: 'k' },
    { chatId: 'b', parentChatId: null, pinned: false, beforeChatId: 'z', groupProjectId: 'h' },
    { chatId: 'c', parentChatId: null, pinned: false, beforeChatId: 'k' },
  ]);
  assert.equal(undo.size, 3);
  await act.undo();
  assert.deepEqual(calls.at(-1), { chatId: 'e', groupProjectId: 'g', parentChatId: null, pinned: false, toEnd: true });
  await act.undo();
  // b sat before b2 under p: undo names that slot.
  assert.deepEqual(calls.at(-1), { chatId: 'b', groupProjectId: 'g', parentChatId: 'p', pinned: false, beforeChatId: 'b2' });
  await act.undo();
  // a was the last top-level chat: undo sends it to the end.
  assert.deepEqual(calls.at(-1), { chatId: 'a', groupProjectId: 'g', parentChatId: null, pinned: false, toEnd: true });
});


test('the tile row: how many tiles fit, what goes under More, and the most-recently-used order', () => {
  // Fit is floor((width + 4) / 60), never below New, Search and More.
  assert.deepEqual([0, 100, 200, 235, 236, 480].map(slotsFit), [3, 3, 3, 3, 4, 8]);
  const views = ['flagged', 'tasks', 'schedules'];
  const split = (fit: number) => {
    const { shown, more } = splitViews(views, fit);
    return `${shown.join(',')}|${more.join(',')}`;
  };
  // At 200 px (3 slots) only New, Search and More show; at 480 px (8 slots) all five tiles do.
  assert.deepEqual([3, 4, 5, 8].map(split), ['|flagged,tasks,schedules', 'flagged|tasks,schedules', 'flagged,tasks,schedules|', 'flagged,tasks,schedules|']);
  assert.deepEqual(splitViews(['flagged'], 3), { shown: ['flagged'], more: [] }, 'one view fits beside New and Search: no More');
  assert.deepEqual(splitViews([], 3), { shown: [], more: [] });

  // The saved order: views that are off are skipped, new ones follow in default order, repeats collapse.
  assert.deepEqual(orderViews(['schedules', 'gone', 'schedules', 'flagged'], ['flagged', 'tasks', 'schedules']), ['schedules', 'flagged', 'tasks']);
  assert.deepEqual(orderViews([], ['flagged', 'tasks']), ['flagged', 'tasks']);
  // Opening a view moves it to the front; an unknown view changes nothing.
  assert.deepEqual(touchView(['flagged', 'tasks', 'schedules'], 'schedules'), ['schedules', 'flagged', 'tasks']);
  assert.deepEqual(touchView(['flagged', 'tasks'], 'schedules'), ['flagged', 'tasks']);
  // Alt+arrow steps one place, and stops at the ends.
  assert.deepEqual(moveView(['flagged', 'tasks', 'schedules'], 'tasks', -1), ['tasks', 'flagged', 'schedules']);
  assert.deepEqual(moveView(['flagged', 'tasks', 'schedules'], 'schedules', 1), ['flagged', 'tasks', 'schedules']);
  // A view that is off right now keeps its saved place behind the visible ones.
  assert.deepEqual(mergeOrder(['tasks', 'flagged', 'schedules'], ['schedules', 'flagged']), ['schedules', 'flagged', 'tasks']);

  // Per device: a damaged or missing value reads as no order, and a blocked store loses nothing else.
  const mem = new Map<string, string>();
  const store = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
  assert.deepEqual(readOrder(store), []);
  writeOrder(['tasks', 'flagged'], store);
  assert.deepEqual(readOrder(store), ['tasks', 'flagged']);
  mem.set('helm.sidebar.tiles', '{not json');
  assert.deepEqual(readOrder(store), []);
  mem.set('helm.sidebar.tiles', '[1,"tasks",null]');
  assert.deepEqual(readOrder(store), ['tasks']);
  assert.doesNotThrow(() => writeOrder(['x'], { getItem: () => null, setItem: () => { throw new Error('full'); } }));
});
