// Item links in the sidebar list, as pure functions.
//
// One chat linked to an item shows as that chat with a small item chip. Two or more chats of one
// item in the same project group show as an item sub-folder titled from the board mirror, or the
// id prefix: a folder row, then those chats one level deeper (an item's main chat first). Only
// top-level chats join a folder; a chat nested under a parent chat stays under its parent.

import type { ItemLink } from '../api/itemlinks.ts';
import type { SidebarRow } from '../api/projects.ts';

/** What a chat shows for its item. */
export interface ItemRef {
  itemId: string;
  /** The board title, or the first 8 characters of the id. */
  title: string;
  isMain: boolean;
}

export const SHORT = 8;

export function itemTitle(l: { itemId: string; itemTitle?: string }): string {
  return l.itemTitle?.trim() ? l.itemTitle : `Item ${l.itemId.slice(0, SHORT)}`;
}

/**
 * The item each chat shows: the one it is main chat of, else the one it linked most recently.
 * A chat linked to several items shows one; the others stay reachable through `item_open`.
 */
export function refsByChat(links: readonly ItemLink[]): Map<string, ItemRef> {
  const best = new Map<string, ItemLink>();
  for (const l of links) {
    const cur = best.get(l.chatId);
    if (!cur || (l.isMain && !cur.isMain) || (l.isMain === cur.isMain && l.linkedAt > cur.linkedAt)) best.set(l.chatId, l);
  }
  const out = new Map<string, ItemRef>();
  for (const [chat, l] of best) out.set(chat, { itemId: l.itemId, title: itemTitle(l), isMain: l.isMain });
  return out;
}

export type Arranged =
  | { type: 'row'; row: SidebarRow; depth: number; chip: ItemRef | null }
  | { type: 'folder'; itemId: string; title: string; members: number; collapsed: boolean; depth: number };

/**
 * Arrange one group's rows (in nesting order) for drawing. Top-level chats that share an item,
 * two or more, move under a folder at the first member's place; a collapsed folder hides them.
 */
export function arrange(
  order: readonly { row: SidebarRow; depth: number }[],
  refs: ReadonlyMap<string, ItemRef> | undefined,
  collapsed: ReadonlySet<string> | undefined,
  maxDepth: number,
  openChatId: string | null = null,
): Arranged[] {
  if (!refs || refs.size === 0) return order.map((o) => ({ type: 'row', row: o.row, depth: o.depth, chip: null }));
  // The top-level members of each item, in drawing order.
  const members = new Map<string, number[]>();
  order.forEach((o, i) => {
    const ref = refs.get(o.row.chatId);
    if (ref && o.depth === 0) members.set(ref.itemId, [...(members.get(ref.itemId) ?? []), i]);
  });
  const folders = new Set([...members].filter(([, ix]) => ix.length >= 2).map(([id]) => id));
  // A member's subtree: the entries after it that are deeper.
  const subtree = (i: number): number[] => {
    const out = [i];
    for (let j = i + 1; j < order.length && order[j]!.depth > 0; j++) out.push(j);
    return out;
  };
  const out: Arranged[] = [];
  const done = new Set<number>();
  const row = (j: number, depth: number, chip: ItemRef | null): Arranged => ({ type: 'row', row: order[j]!.row, depth: Math.min(depth, maxDepth), chip });
  for (let i = 0; i < order.length; i++) {
    if (done.has(i)) continue;
    const ref = refs.get(order[i]!.row.chatId);
    if (ref && order[i]!.depth === 0 && folders.has(ref.itemId)) {
      const ix = members.get(ref.itemId)!;
      // The open chat's folder stays open, so the chat is visible.
      const isCollapsed = (collapsed?.has(ref.itemId) ?? false) && !ix.some((m) => order[m]!.row.chatId === openChatId);
      out.push({ type: 'folder', itemId: ref.itemId, title: ref.title, members: ix.length, collapsed: isCollapsed, depth: 0 });
      // The item's main chat first, then the rest in their order.
      const sorted = [...ix].sort((a, b) => Number(refs.get(order[b]!.row.chatId)!.isMain) - Number(refs.get(order[a]!.row.chatId)!.isMain) || a - b);
      for (const m of sorted) {
        for (const j of subtree(m)) {
          done.add(j);
          if (!isCollapsed) out.push(row(j, order[j]!.depth + 1, null));
        }
      }
      continue;
    }
    done.add(i);
    out.push(row(i, order[i]!.depth, ref && order[i]!.depth === 0 ? ref : null));
  }
  return out;
}
