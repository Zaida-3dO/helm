import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { toast } from '../ui/toasts.ts';
import { archiveWhenIdle, SidebarActions, type ActionDeps } from './actions.ts';
import { UndoStack } from './undo.ts';
import { buildList, chatAncestors, headerSubtitle, HEIGHTS, nestRows, stepChat, visibleChatOrder, type ListItem } from './model.ts';
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
  assert.deepEqual(header(items, 'a').marks, { running: 2, unread: 1 });
  assert.equal(header(items, 'open').marks, null, 'an expanded group shows its own rows instead');
  assert.equal(header(buildList([project('b', { collapsed: true, chatCount: 1 })], [row('x', 'b')], opts), 'b').marks, null, 'nothing going on, no marks');
});

test('a missing parent draws at the top level and a collapsed group hides its contents', () => {
  const projects = [project('a', { parentId: 'gone', chatCount: 1 }), project('b', { collapsed: true }), project('c', { parentId: 'b', chatCount: 1 })];
  const rows = [row('x', 'b')];
  assert.deepEqual(shape(buildList(projects, rows, opts)), ['H0:a', 'H0:b']);
  // The open chat's group stays expanded.
  assert.deepEqual(shape(buildList(projects, rows, { ...opts, selectedChatId: 'x' })), ['H0:a', 'H0:b', 'H1:c', 'C0:x']);
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
    project('pinned', { pinned: true }),
    project('open'),
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
  // The open chat's group shows even when it holds nothing else (here its chat is archived).
  assert.ok(shape(buildList(projects, rows, { ...opts, selectedChatId: 'o1' })).includes('H0:open'));
  // A shown finished project says so under its name; a stale one still reads as gone.
  const shown = buildList(projects, rows, opts);
  const header = (id: string) => shown.find((i): i is Extract<ListItem, { kind: 'header' }> => i.kind === 'header' && i.project.id === id)!;
  assert.equal(header('done').subtitle, 'Finished');
  assert.equal(header('done').height, HEIGHTS.normal.headerSubtitle);
  assert.equal(header('live').subtitle, null);
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
    ops: { archive: async (id: string) => void log.push(`archive ${id}`), restore: async (id: string) => void log.push(`restore ${id}`) },
    pops: { archive: async (id: string) => void log.push(`archive-project ${id}`), restore: async (id: string) => void log.push(`restore-project ${id}`) },
    data: { refetch: async () => undefined, loadProjects: async () => undefined, invalidatePaging: () => undefined },
  } as unknown as ActionDeps);
  await act.archive({ chatId: 'c1', title: 'Plans' } as SidebarRow);
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
  const act = new SidebarActions({
    undo,
    onError: (m: string) => calls.push(`error ${m}`),
    pops: { organize: async (a: unknown) => (calls.push(a), []) },
    // Top level of g, newest first: c, a (so a is followed by nothing); b is the only child of p.
    data: { mergeRows: () => undefined, getSnapshot: () => ({ rows: held }) },
  } as unknown as ActionDeps);
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

test('archiving after a stop retries only while the chat still counts as working', async () => {
  const tries = (fail: string | null, times: number) => {
    let n = 0;
    const ops = {
      archive: async () => {
        n++;
        if (fail && n <= times) throw new Error(fail);
      },
    };
    return { ops: ops as never, count: () => n };
  };
  const busy = 'This chat is working; stop the task first, then archive it.';
  const a = tries(busy, 2);
  await archiveWhenIdle(a.ops, 'c1', 5, 1);
  assert.equal(a.count(), 3, 'retried until the stop landed');
  const b = tries('The chat store is locked.', 9);
  await assert.rejects(archiveWhenIdle(b.ops, 'c1', 5, 1), /locked/);
  assert.equal(b.count(), 1, 'any other refusal is final at once');
  const c = tries(busy, 99);
  await assert.rejects(archiveWhenIdle(c.ops, 'c1', 4, 1), /working/);
  assert.equal(c.count(), 4, 'gives up after the last try');
});
