// The dev fake host's item-link ops (the sidebar's item chips, sub-folders and picker in a plain
// browser). With `?items=1` in the page URL it links the first two chats it is asked about to one
// board project (so they form a sub-folder, the second one main) and the third chat to another
// item (a single chat with a titled chip) and the fourth to a third item with no title (a glyph). Dev builds only.

import { ApiError } from '../api/client.ts';
import type { ItemLink } from '../api/itemlinks.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const ITEM_A = 'demo0001d8324d0b8c1f34dd7d21a910';
const ITEM_B = '50f653d8aaaabbbbccccddddeeee0000';
const ITEM_C = '7c1e9a42bbbbccccddddeeeeffff1111';

export function itemLinksFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const on = typeof location !== 'undefined' && new URLSearchParams(location.search).get('items') === '1';
  let mainA = 1;
  const links = (chatIds: string[], cwdOf: (id: string) => string): ItemLink[] => {
    if (!on) return [];
    const out: ItemLink[] = [];
    // The chats of the project folder holding the most of them, so two of them form a sub-folder.
    const byCwd = new Map<string, string[]>();
    for (const id of chatIds) byCwd.set(cwdOf(id), [...(byCwd.get(cwdOf(id)) ?? []), id]);
    const [a, b, c, d] = [...byCwd.values()].sort((x, y) => y.length - x.length)[0] ?? [];
    const base = Date.now() - 3_600_000;
    if (a) out.push({ itemId: ITEM_A, chatId: a, isMain: mainA === 0, linkedBy: 'agent', linkedAt: base, itemTitle: 'Usage meter' });
    if (b) out.push({ itemId: ITEM_A, chatId: b, isMain: mainA === 1, linkedBy: 'agent', linkedAt: base + 1000, itemTitle: 'Usage meter' });
    if (c) out.push({ itemId: ITEM_B, chatId: c, isMain: true, linkedBy: 'agent', linkedAt: base + 2000, itemTitle: 'Fix the merge queue' });
    if (d) out.push({ itemId: ITEM_C, chatId: d, isMain: true, linkedBy: 'agent', linkedAt: base + 3000 });
    return out;
  };
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'item_links_for_chats':
        {
          const cwds = new Map((ctx.call('chat_list', {}) as { id: string; cwd: string }[]).map((c) => [c.id, c.cwd]));
          return { links: links((a.chatIds as string[]) ?? [], (id) => cwds.get(id) ?? '') };
        }
      case 'item_links_for_item':
        return { links: [] };
      case 'item_link_add':
        ctx.emit({ kind: 'item_links', itemIds: [String(a.itemId)] });
        return { links: [] };
      case 'item_link_set_main': {
        mainA = (a.chatId as string) === String((ctx.call('chat_list', {}) as { id: string }[])[0]?.id) ? 0 : 1;
        ctx.emit({ kind: 'item_links', itemIds: [ITEM_A] });
        return { links: [] };
      }
      case 'item_open': {
        const all = ctx.call('chat_list', {}) as { id: string; title: string; cwd: string }[];
        const counts = new Map<string, number>();
        for (const c of all) counts.set(c.cwd, (counts.get(c.cwd) ?? 0) + 1);
        const top = [...counts.entries()].sort((x, y) => y[1] - x[1])[0]?.[0];
        const chats = all.filter((c) => c.cwd === top).slice(0, 4);
        const id = String(a.itemId).toLowerCase().replace(/-/g, '');
        if (id.length < 8) throw new ApiError('bad_args', 'That is not an item id.');
        if (ITEM_A.startsWith(id) && chats.length >= 2) {
          const [x, y] = [chats[0]!, chats[1]!];
          const ordered = mainA === 1 ? [y, x] : [x, y];
          return {
            itemId: ITEM_A,
            itemTitle: 'Usage meter',
            candidates: ordered.map((c, i) => ({ chatId: c.id, title: c.title, isMain: i === 0, archived: false, linkedAt: Date.now() })),
          };
        }
        if (ITEM_C.startsWith(id) && chats[3]) return { itemId: ITEM_C, chatId: chats[3].id, candidates: [] };
        if (ITEM_B.startsWith(id) && chats[2]) return { itemId: ITEM_B, chatId: chats[2].id, candidates: [] };
        throw new ApiError('not_found', 'No chat is linked to this item.');
      }
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['item_'], handler };
}
