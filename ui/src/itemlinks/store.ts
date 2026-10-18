// The sidebar's item-link data: the links of the chats it lists, re-read when the host says
// `item_links` changed or the set of chats changes. Plain TypeScript, tested under node:test.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { itemLinkOps, type ItemLink, type ItemOpen } from '../api/itemlinks.ts';
import { refsByChat, type ItemRef } from './folders.ts';

const MAX_CHATS = 2_000;

type Listener = () => void;

export class ItemLinksStore {
  private links: ItemLink[] = [];
  private refs: ReadonlyMap<string, ItemRef> = new Map();
  private ids: string[] = [];
  private key = '';
  private readonly listeners = new Set<Listener>();
  private readonly ops: ReturnType<typeof itemLinkOps>;
  private off: (() => void) | undefined;
  private seq = 0;

  private readonly bus: EventBus;

  constructor(api: Api, bus: EventBus) {
    this.bus = bus;
    this.ops = itemLinkOps(api);
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): ReadonlyMap<string, ItemRef> {
    return this.refs;
  }

  get all(): readonly ItemLink[] {
    return this.links;
  }

  start(): void {
    this.off ??= this.bus.on('item_links', () => void this.load());
  }

  stop(): void {
    this.off?.();
    this.off = undefined;
  }

  /** Tell the store which chats the sidebar lists; reads only when that set changed. */
  setChats(chatIds: readonly string[]): void {
    const ids = chatIds.slice(0, MAX_CHATS);
    const key = ids.join(',');
    if (key === this.key) return;
    this.key = key;
    this.ids = ids;
    void this.load();
  }

  async load(): Promise<void> {
    const mine = ++this.seq;
    if (this.ids.length === 0) return this.apply(mine, []);
    try {
      this.apply(mine, await this.ops.forChats(this.ids));
    } catch {
      // The links are decoration: a failed read leaves the last ones in place.
    }
  }

  private apply(seq: number, links: ItemLink[]): void {
    if (seq !== this.seq) return; // a newer read is on its way
    this.links = links;
    this.refs = refsByChat(links);
    for (const l of [...this.listeners]) l();
  }
}

/** The one place an "open this item" request lands: a chat to open, or chats to choose from. */
export class ItemPicker {
  private state: ItemOpen | null = null;
  private readonly listeners = new Set<Listener>();

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get current(): ItemOpen | null {
    return this.state;
  }

  show(o: ItemOpen | null): void {
    this.state = o;
    for (const l of [...this.listeners]) l();
  }
}

export const itemPicker = new ItemPicker();

/** Open an item's chat: the only one, the main one is first in the picker when there are several. */
export async function openItem(api: Api, select: (chatId: string) => void, itemId: string): Promise<string | null> {
  try {
    const r = await itemLinkOps(api).open(itemId);
    if (r.chatId) {
      select(r.chatId);
      return null;
    }
    itemPicker.show(r);
    return null;
  } catch (e) {
    return String((e as Error)?.message ?? e);
  }
}

const stores = new WeakMap<Api, ItemLinksStore>();

export function itemLinksStoreFor(api: Api, bus: EventBus): ItemLinksStore {
  let s = stores.get(api);
  if (!s) {
    s = new ItemLinksStore(api, bus);
    stores.set(api, s);
  }
  return s;
}
