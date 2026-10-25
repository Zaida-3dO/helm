import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import type { ItemLink, ItemOpen } from '../api/itemlinks.ts';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { buildList } from '../sidebar/model.ts';
import { arrange, itemMarkLabel, itemTitle, MAX_TITLE, refsByChat } from './folders.ts';
import { ItemLinksStore, ItemPicker, itemPicker, openItem } from './store.ts';

const A = 'demo0001d8324d0b8c1f34dd7d21a910';
const B = '50f653d8aaaabbbbccccddddeeee0000';

function row(id: string, over: Partial<SidebarRow> = {}): SidebarRow {
  return {
    chatId: id, title: `Chat ${id}`, groupProjectId: 'g', folderProjectId: 'g', folderPath: 'C:/w', depth: 0, parentChatId: null, hasChildren: false,
    status: 'idle', attention: null, pinned: false, unread: false, muted: false, stalled: false, hasDraft: false, lastActivityAt: 1, archived: false, position: null,
    createdAt: 1, updatedAt: 1, revision: 1, ...over,
  } as SidebarRow;
}
const link = (item: string, chat: string, over: Partial<ItemLink> = {}): ItemLink => ({ itemId: item, chatId: chat, isMain: false, linkedBy: 'agent', linkedAt: 1, ...over });
const group = (): ProjectNode => ({ id: 'g', kind: 'folder', name: 'G', path: null, parentId: null, parentSource: null, icon: null, colour: null, pinned: false, collapsed: false, position: null, archived: false, board: null, primaryFolderProjectId: null, chatCount: 0, createdAt: 1, revision: 0 }) as ProjectNode;

test('the title is the board title, else "Linked item"; the mark says what it is, with or without a title', () => {
  assert.equal(itemTitle({ itemId: A, itemTitle: 'Usage meter' }), 'Usage meter');
  assert.equal(itemTitle({ itemId: A }), 'Linked item');
  assert.equal(itemTitle({ itemId: A, itemTitle: '  ' }), 'Linked item');
  assert.equal(itemTitle({ itemId: A, itemTitle: 'x'.repeat(500) }).length, MAX_TITLE, 'outside text is cut');
  const named = refsByChat([link(A, 'a', { itemTitle: 'Fix the deploy' }), link(B, 'b')]);
  assert.equal(itemMarkLabel(named.get('a')!), 'Linked to board item: Fix the deploy. Click to open');
  assert.equal(itemMarkLabel(named.get('b')!), 'Linked to a board item. Click to open', 'no title: no bare id either');
});

test('a chat shows the item it is main chat of, else its newest link', () => {
  const refs = refsByChat([link(A, 'c', { linkedAt: 5 }), link(B, 'c', { linkedAt: 9 }), link(A, 'd', { linkedAt: 1 }), link(B, 'd', { linkedAt: 2, isMain: false })]);
  assert.equal(refs.get('c')?.itemId, B, 'the newest link wins when neither is main');
  assert.equal(refreshed(refsByChat([link(A, 'c', { isMain: true, linkedAt: 1 }), link(B, 'c', { linkedAt: 9 })]), 'c'), A, 'main beats newer');
});
const refreshed = (m: ReadonlyMap<string, { itemId: string }>, c: string) => m.get(c)?.itemId;

test('one linked chat gets a chip and no folder; two or more become a folder with the main chat first', () => {
  const order = ['a', 'b', 'c', 'x'].map((id) => ({ row: row(id), depth: 0 }));
  const one = arrange(order, refsByChat([link(A, 'a')]), undefined, 2);
  assert.deepEqual(one.map((e) => e.type), ['row', 'row', 'row', 'row']);
  assert.equal((one[0] as { chip: unknown }).chip !== null, true);
  const two = arrange(order, refsByChat([link(A, 'a'), link(A, 'c', { isMain: true }), link(B, 'x')]), undefined, 2);
  assert.deepEqual(two.map((e) => (e.type === 'folder' ? `F:${e.members}` : `${e.row.chatId}@${e.depth}`)), ['F:2', 'c@1', 'a@1', 'b@0', 'x@0']);
  assert.equal((two[4] as { chip: unknown }).chip !== null, true, 'a lone linked chat keeps its chip');
  assert.equal((two[1] as { chip: unknown }).chip, null, 'inside a folder the folder says the item');
});

test('a collapsed folder hides its chats, except while one of them is open', () => {
  const order = ['a', 'b'].map((id) => ({ row: row(id), depth: 0 }));
  const refs = refsByChat([link(A, 'a'), link(A, 'b')]);
  const hidden = arrange(order, refs, new Set([A]), 2);
  assert.deepEqual(hidden.map((e) => e.type), ['folder']);
  assert.equal((hidden[0] as { collapsed: boolean }).collapsed, true);
  const open = arrange(order, refs, new Set([A]), 2, 'b');
  assert.deepEqual(open.map((e) => e.type), ['folder', 'row', 'row']);
});

test('only top-level chats join a folder; a child chat stays under its parent', () => {
  const order = [
    { row: row('p'), depth: 0 },
    { row: row('k', { parentChatId: 'p' }), depth: 1 },
    { row: row('q'), depth: 0 },
  ];
  const out = arrange(order, refsByChat([link(A, 'p'), link(A, 'k'), link(A, 'q')]), undefined, 2);
  assert.deepEqual(out.map((e) => (e.type === 'folder' ? `F:${e.members}` : `${e.row.chatId}@${e.depth}`)), ['F:2', 'p@1', 'k@2', 'q@1'], 'the subtree moves with its parent');
});

test('buildList draws the folder as one list item and keeps chat heights', () => {
  const rows = [row('a'), row('b'), row('c')];
  const refs = refsByChat([link(A, 'a'), link(A, 'b')]);
  const list = buildList([group()], rows, { view: 'projects', compact: false, expandedChats: new Set(), selectedChatId: null, itemLinks: { refs, collapsed: new Set() } });
  assert.deepEqual(list.map((i) => i.kind), ['header', 'itemFolder', 'chat', 'chat', 'chat']);
  const folder = list[1] as { members: number; title: string; height: number };
  assert.equal(folder.members, 2);
  assert.equal(folder.title, 'Linked item');
  assert.equal(folder.height, (list[2] as { height: number }).height);
  const none = buildList([group()], rows, { view: 'projects', compact: false, expandedChats: new Set(), selectedChatId: null });
  assert.deepEqual(none.map((i) => i.kind), ['header', 'chat', 'chat', 'chat'], 'no links: the list is as before');
});

test('the store reads once per set of chats, again on the item_links event, and drops a stale answer', async () => {
  const bus = new EventBus();
  const calls: string[][] = [];
  let release: (() => void) | undefined;
  const api = (async (op: string, args: { chatIds: string[] }) => {
    assert.equal(op, 'item_links_for_chats');
    calls.push(args.chatIds);
    if (calls.length === 1) await new Promise<void>((r) => (release = r));
    return { links: calls.length === 1 ? [link(A, 'a')] : [link(B, 'a')] };
  }) as never;
  const store = new ItemLinksStore(api, bus);
  store.start();
  store.setChats(['a', 'b']);
  store.setChats(['a', 'b']);
  assert.equal(calls.length, 1, 'the same set is not read twice');
  bus.dispatch({ kind: 'item_links', itemIds: [A] });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(calls.length, 2, 'the event reads again');
  release?.();
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.snapshot.get('a')?.itemId, B, 'the first, slower answer did not overwrite the newer one');
  store.stop();
});

test('open: one chat is selected; several go to the picker, main first; a refusal comes back as text', async () => {
  const picked: string[] = [];
  const one: ItemOpen = { itemId: A, chatId: 'only', candidates: [] };
  assert.equal(await openItem((async () => one) as never, (id) => picked.push(id), A), null);
  assert.deepEqual(picked, ['only']);
  const many: ItemOpen = { itemId: A, candidates: [{ chatId: 'm', title: 'Main', isMain: true, archived: false, linkedAt: 2 }, { chatId: 'n', title: 'Other', isMain: false, archived: false, linkedAt: 1 }] };
  let seen: ItemOpen | null = null;
  const off = itemPicker.subscribe(() => (seen = itemPicker.current));
  assert.equal(await openItem((async () => many) as never, (id) => picked.push(id), A), null);
  assert.deepEqual(picked, ['only'], 'nothing is opened until the user picks');
  assert.equal((seen as ItemOpen | null)?.candidates[0]?.chatId, 'm');
  off();
  itemPicker.show(null);
  const err = await openItem((async () => { throw new Error('No chat is linked to this item.'); }) as never, () => {}, A);
  assert.equal(err, 'No chat is linked to this item.');
  assert.ok(new ItemPicker().current === null);
});
