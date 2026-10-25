import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { canSitUnder, dropAction, passedThreshold } from './drag.ts';
import { chatMenuItems, nestTargets, projectMenuItems, projectNestTargets, type ChatMenuActions, type ProjectMenuActions } from './menus.ts';
import { UNDO_LIMIT, UndoStack } from './undo.ts';

const noop = () => undefined;
const chatActs: ChatMenuActions = { fork: noop, rename: noop, compact: noop, rollOver: noop, moveTo: noop, nestUnder: noop, topLevel: noop, toggleChildren: noop, markUnread: noop, pin: noop, mute: noop, archive: noop, restore: noop, remove: noop };
const projActs: ProjectMenuActions = { newChat: noop, pin: noop, rename: noop, changeIcon: noop, nestUnder: noop, topLevel: noop, archive: noop, restore: noop, colour: noop, customColour: noop };

function row(chatId: string, over: Partial<SidebarRow> = {}): SidebarRow {
  return { chatId, title: chatId, groupProjectId: 'g', folderProjectId: 'g', folderPath: 'C:/w', depth: 0, parentChatId: null, hasChildren: false, status: 'idle', attention: null, pinned: false, unread: false, muted: false, stalled: false, hasDraft: false, lastActivityAt: 1, archived: false, position: null, createdAt: 1, updatedAt: 1, revision: 1, ...over };
}

function project(id: string, over: Partial<ProjectNode> = {}): ProjectNode {
  return { id, kind: 'folder', name: id, path: null, parentId: null, parentSource: null, icon: null, colour: null, pinned: false, collapsed: false, position: null, archived: false, board: null, primaryFolderProjectId: null, chatCount: 0, createdAt: 1, revision: 0, ...over };
}

test('drag starts only after 4 px', () => {
  assert.equal(passedThreshold(3, 0), false);
  assert.equal(passedThreshold(2, 3), false);
  assert.equal(passedThreshold(4, 0), true);
});

test('drop decisions: onto a group, before a chat at its level, under a chat; levels a chat may not take are not offered', () => {
  const src = { kind: 'chat' as const, chatId: 'a', groupId: 'g', pinned: false };
  assert.deepEqual(dropAction(src, { kind: 'header', projectId: 'h', parentId: null, pinned: false, lowerHalf: false }), { kind: 'chat-to-group', chatId: 'a', groupId: 'h' });
  assert.equal(dropAction(src, { kind: 'header', projectId: 'g', parentId: null, pinned: false, lowerHalf: false }), null, 'its own group changes nothing');
  assert.deepEqual(dropAction(src, { kind: 'chat', chatId: 'b', groupId: 'g', pinned: false, lowerHalf: false }), { kind: 'chat-before', chatId: 'a', beforeChatId: 'b', groupId: 'g', moveGroup: false, parentChatId: null, pinned: false });
  assert.deepEqual(dropAction(src, { kind: 'chat', chatId: 'b', groupId: 'g', pinned: false, lowerHalf: true }), { kind: 'chat-nest', chatId: 'a', parentChatId: 'b', groupId: 'g', moveGroup: false });
  // Before a nested child or a pinned row: the chat takes that row's parent and pin state.
  assert.deepEqual(dropAction(src, { kind: 'chat', chatId: 'k', groupId: 'g', pinned: false, lowerHalf: false, parentChatId: 'b', levelOk: true }), { kind: 'chat-before', chatId: 'a', beforeChatId: 'k', groupId: 'g', moveGroup: false, parentChatId: 'b', pinned: false });
  assert.deepEqual(dropAction(src, { kind: 'chat', chatId: 'b', groupId: 'h', pinned: true, lowerHalf: false, parentChatId: null }), { kind: 'chat-before', chatId: 'a', beforeChatId: 'b', groupId: 'h', moveGroup: true, parentChatId: null, pinned: true });
  assert.equal(dropAction(src, { kind: 'chat', chatId: 'k', groupId: 'g', pinned: false, lowerHalf: false, parentChatId: 'b', levelOk: false }), null, 'a level it may not take');
  assert.equal(dropAction(src, { kind: 'chat', chatId: 'b', groupId: 'g', pinned: false, lowerHalf: true, nestOk: false }), null, 'no nesting where it does not fit');
  assert.equal(dropAction(src, { kind: 'chat', chatId: 'a', groupId: 'g', pinned: false, lowerHalf: true }), null, 'not onto itself');
  const p = { kind: 'project' as const, projectId: 'p', parentId: null, pinned: false };
  assert.deepEqual(dropAction(p, { kind: 'header', projectId: 'q', parentId: null, pinned: false, lowerHalf: false }), { kind: 'project-before', projectId: 'p', beforeProjectId: 'q' });
  assert.deepEqual(dropAction(p, { kind: 'header', projectId: 'q', parentId: 'x', pinned: false, lowerHalf: true }), { kind: 'project-nest', projectId: 'p', parentId: 'q' });
  assert.equal(dropAction(p, { kind: 'header', projectId: 'q', parentId: 'x', pinned: false, lowerHalf: false }), null, 'reorder only among siblings');
  assert.equal(dropAction(p, { kind: 'chat', chatId: 'c', groupId: 'g', pinned: false, lowerHalf: false }), null);
});

test('a chat may sit under a parent unless that is itself, its own started chat, or past three levels', () => {
  const rows = [row('a'), row('b', { parentChatId: 'a' }), row('c', { parentChatId: 'b' }), row('d'), row('e', { parentChatId: 'd' })];
  const table: [string, string | null, boolean][] = [
    ['d', null, true],
    ['c', null, true],
    ['d', 'a', true],
    ['d', 'b', false], // d carries e: three levels
    ['d', 'c', false], // c is already two levels down
    ['a', 'c', false], // under its own started chat
    ['a', 'a', false],
    ['a', 'd', false], // a carries two levels: it can only stay at the top
    ['e', 'b', true],
    ['c', 'a', true],
  ];
  for (const [chat, parent, ok] of table) assert.equal(canSitUnder(rows, chat, parent), ok, `${chat} under ${parent}`);
});

test('the undo stack holds 50, undoes newest first, and a new action clears redo', async () => {
  const s = new UndoStack();
  const log: string[] = [];
  for (let i = 0; i < UNDO_LIMIT + 5; i++) {
    s.push({ label: String(i), undo: async () => void log.push(`u${i}`), redo: async () => void log.push(`r${i}`) });
  }
  assert.equal(s.size, UNDO_LIMIT);
  await s.undo();
  await s.undo();
  assert.deepEqual(log, ['u54', 'u53']);
  await s.redo();
  assert.deepEqual(log.at(-1), 'r53');
  s.push({ label: 'x', undo: async () => undefined, redo: async () => undefined });
  assert.equal(s.canRedo(), false);
  for (let i = 0; i < UNDO_LIMIT; i++) await s.undo();
  assert.equal(await s.undo(), null);
});

test('nest targets exclude itself, its descendants, its parent, other groups and over-depth', () => {
  const rows = [
    row('a'),
    row('b', { parentChatId: 'a' }),
    row('c', { parentChatId: 'b' }),
    row('d'),
    row('e', { groupProjectId: 'other' }),
    row('f', { archived: true }),
  ];
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  const depthOf = (id: string) => {
    let d = 0;
    let cur = byId.get(id)?.parentChatId;
    while (cur) {
      d++;
      cur = byId.get(cur)?.parentChatId;
    }
    return d;
  };
  const heightOf = (id: string): number => {
    const kids = rows.filter((r) => r.parentChatId === id);
    return kids.length ? 1 + Math.max(...kids.map((k) => heightOf(k.chatId))) : 0;
  };
  const input = (r: SidebarRow) => ({ row: r, projects: [project('g')], siblings: rows, heightOf, depthOf, expanded: false, view: 'projects' as const });
  assert.deepEqual(nestTargets(input(byId.get('d')!)).map((r) => r.chatId), ['a', 'b'], 'c is at depth 2: nothing may go under it');
  assert.deepEqual(nestTargets(input(byId.get('a')!)).map((r) => r.chatId), [], 'a has two levels below it: it can only stay top level');
  assert.deepEqual(nestTargets(input(byId.get('c')!)).map((r) => r.chatId), ['a', 'd'], 'not its own parent b');
});

test('the chat menu follows state: archive or restore and delete, stop first while busy', () => {
  const ids = (r: SidebarRow) => chatMenuItems({ row: r, projects: [project('g')], siblings: [r], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'projects' }, chatActs).map((i) => i.id);
  assert.ok(ids(row('a')).includes('archive'));
  // Compact context: offered on an idle chat, not while it runs or once archived.
  const compact = (r: SidebarRow) =>
    chatMenuItems({ row: r, projects: [project('g')], siblings: [r], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'projects' }, chatActs).find((i) => i.id === 'compact');
  assert.equal(compact(row('a'))?.disabled, false);
  assert.equal(compact(row('a', { status: 'running' }))?.disabled, true);
  assert.equal(compact(row('a', { archived: true }))?.disabled, true);
  // Roll over: the same rule, and it runs the action the menu was given.
  let rolled = 0;
  const rollOver = (r: SidebarRow) =>
    chatMenuItems({ row: r, projects: [project('g')], siblings: [r], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'projects' }, { ...chatActs, rollOver: () => void rolled++ }).find((i) => i.id === 'rollover');
  assert.equal(rollOver(row('a'))?.disabled, false);
  assert.equal(rollOver(row('a', { status: 'running' }))?.disabled, true);
  assert.equal(rollOver(row('a', { archived: true }))?.disabled, true);
  rollOver(row('a'))?.onSelect?.();
  assert.equal(rolled, 1);
  assert.ok(!ids(row('a')).includes('delete'));
  const archived = ids(row('a', { archived: true }));
  assert.ok(archived.includes('restore') && archived.includes('delete'));
  const busy = chatMenuItems({ row: row('a', { status: 'running' }), projects: [project('g')], siblings: [], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'projects' }, chatActs);
  assert.deepEqual(busy.find((i) => i.id === 'archive')?.label, 'Stop and archive…', 'a busy chat can be archived through the stop dialog');
  assert.ok(ids(row('a', { parentChatId: 'p' })).includes('top'));
  const act = chatMenuItems({ row: row('a'), projects: [project('g')], siblings: [], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'activity' }, chatActs).map((i) => i.id);
  assert.ok(!act.includes('nest'), 'no nesting in Activity');
  const pin = chatMenuItems({ row: row('a', { pinned: true }), projects: [project('g')], siblings: [], heightOf: () => 0, depthOf: () => 0, expanded: false, view: 'projects' }, chatActs).find((i) => i.id === 'pin');
  assert.equal(pin?.label, 'Unpin chat');
});

test('project nest targets keep three levels and refuse cycles', () => {
  const all = [project('a'), project('b', { parentId: 'a' }), project('c', { parentId: 'b' }), project('d'), project('x', { archived: true })];
  const names = (p: ProjectNode) => projectNestTargets(p, all).map((x) => x.id);
  assert.deepEqual(names(all[3]!), ['a', 'b'], 'not under c (depth 2), not archived x');
  assert.deepEqual(names(all[0]!), [], 'a carries two levels');
  assert.deepEqual(names(all[2]!), ['a', 'd'], 'not its own parent b');
  const items = projectMenuItems(all[1]!, all, projActs).map((i) => i.id);
  assert.ok(items.includes('top') && items.includes('colour') && items.includes('archive'));
  const colours = projectMenuItems(all[1]!, all, projActs).find((i) => i.id === 'colour')!.submenu!;
  assert.equal(colours.length, 10, 'no colour, the eight named colours and a custom one');
});

test('the project menu places Primary folder… right after Rename', () => {
  const ids = projectMenuItems(project('a'), [project('a')], projActs).map((i) => i.id);
  assert.equal(ids[ids.indexOf('rename') + 1], 'primary-folder');
});
