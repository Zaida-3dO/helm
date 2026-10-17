// Batches turn-change requests: every card asks for its own row, and the loader sends
// one `review_turns` per chat 150 ms after the last ask, at most 600 ids, one request at a time.
// Answers are cached per (chat, message, revision), so a re-render never asks again.

import type { TurnChanges } from '../../api/panel.ts';

export const BATCH_MS = 150;
export const MAX_IDS = 600;

type Fetch = (chatId: string, ids: number[]) => Promise<TurnChanges[]>;

interface Pending {
  ids: Map<number, number>;
  waiters: Array<() => void>;
  timer: ReturnType<typeof setTimeout> | null;
  inFlight: boolean;
}

export class TurnLoader {
  private readonly cache = new Map<string, TurnChanges | null>();
  private readonly pending = new Map<string, Pending>();
  private readonly fetch: Fetch;
  private readonly delay: number;

  constructor(fetch: Fetch, delay = BATCH_MS) {
    this.fetch = fetch;
    this.delay = delay;
  }

  private key(chatId: string, id: number, revision: number) {
    return `${chatId}\u0000${id}\u0000${revision}`;
  }

  /** The cached answer: a value, null (no card for this row), or undefined (not asked yet). */
  peek(chatId: string, id: number, revision: number): TurnChanges | null | undefined {
    return this.cache.get(this.key(chatId, id, revision));
  }

  /** Resolves once the row's answer is cached. */
  load(chatId: string, id: number, revision: number): Promise<void> {
    if (this.cache.has(this.key(chatId, id, revision))) return Promise.resolve();
    let p = this.pending.get(chatId);
    if (!p) this.pending.set(chatId, (p = { ids: new Map(), waiters: [], timer: null, inFlight: false }));
    p.ids.set(id, revision);
    const pend = p;
    return new Promise<void>((resolve) => {
      pend.waiters.push(resolve);
      this.schedule(chatId, pend);
    });
  }

  private schedule(chatId: string, p: Pending) {
    if (p.timer) clearTimeout(p.timer);
    p.timer = setTimeout(() => void this.flush(chatId, p), this.delay);
  }

  private async flush(chatId: string, p: Pending) {
    p.timer = null;
    if (p.inFlight) {
      this.schedule(chatId, p);
      return;
    }
    const batch = [...p.ids].slice(0, MAX_IDS);
    for (const [id] of batch) p.ids.delete(id);
    const waiters = p.ids.size === 0 ? p.waiters.splice(0) : [];
    p.inFlight = true;
    try {
      const out = await this.fetch(chatId, batch.map(([id]) => id));
      const byId = new Map(out.map((t) => [t.messageId, t]));
      for (const [id, rev] of batch) this.cache.set(this.key(chatId, id, rev), byId.get(id) ?? null);
    } catch {
      // A failure is not an answer: nothing is cached, so the next mount asks again.
    } finally {
      p.inFlight = false;
      waiters.forEach((w) => w());
      if (p.ids.size > 0) this.schedule(chatId, p);
    }
  }
}
