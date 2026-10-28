// Typed wrappers over the item-link ops. Every call still goes through
// api(op, args). Links live only in Helm; nothing here reaches the board.

import type { Api } from './client.ts';
import type { ItemLink, ItemLinksForChats, ItemOpen } from './generated/itemlinks/index.ts';

export type { ItemCandidate, ItemLink, ItemLinksForChats, ItemOpen, LinkedBy } from './generated/itemlinks/index.ts';

export function itemLinkOps(api: Api) {
  return {
    forChats: async (chatIds: string[]): Promise<ItemLink[]> => (await api<ItemLinksForChats>('item_links_for_chats', { chatIds })).links,
    forItem: async (itemId: string): Promise<ItemLink[]> => (await api<ItemLinksForChats>('item_links_for_item', { itemId })).links,
    setMain: async (itemId: string, chatId: string): Promise<ItemLink[]> => (await api<ItemLinksForChats>('item_link_set_main', { itemId, chatId })).links,
    add: async (itemId: string, chatId: string, title?: string): Promise<ItemLink[]> =>
      (await api<ItemLinksForChats>('item_link_add', { itemId, chatId, ...(title ? { title } : {}) })).links,
    open: (itemId: string) => api<ItemOpen>('item_open', { itemId }),
  };
}

export type ItemLinkOps = ReturnType<typeof itemLinkOps>;
