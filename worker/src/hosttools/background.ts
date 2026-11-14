// Calls of the helm server that were moved to the background.
//
// A host-tool or bridged MCP call that runs long is answered early, so Claude Code reaches its
// next tool boundary and reads any message waiting for it. The real request keeps running and
// lands here; the agent reads it with `app_background_result`. The store is small and forgets on
// its own: at most MAX_CALLS calls, a result is dropped READ_KEEP_MS after it was read or
// DONE_KEEP_MS after it finished, whichever comes first. Running calls are never dropped, so a
// store full of running calls takes no more (the next slow call simply keeps the agent waiting).

import type { CallResult } from "./channel.ts";

export const MAX_CALLS = 32;
export const READ_KEEP_MS = 10 * 60_000;
export const DONE_KEEP_MS = 30 * 60_000;

export interface BackgroundCall {
  callId: string;
  tool: string;
  mutating: boolean;
  startedAt: number;
  /** When it was moved to the background. */
  movedAt: number;
  finishedAt?: number;
  readAt?: number;
  result?: CallResult;
  /** Settles when the call finishes (never rejects). */
  done: Promise<void>;
}

export interface StoreOptions {
  max?: number;
  readKeepMs?: number;
  doneKeepMs?: number;
  now?: () => number;
}

export class BackgroundStore {
  private readonly calls = new Map<string, BackgroundCall>();
  private readonly max: number;
  private readonly readKeepMs: number;
  private readonly doneKeepMs: number;
  private readonly now: () => number;

  constructor(o: StoreOptions = {}) {
    this.max = o.max ?? MAX_CALLS;
    this.readKeepMs = o.readKeepMs ?? READ_KEEP_MS;
    this.doneKeepMs = o.doneKeepMs ?? DONE_KEEP_MS;
    this.now = o.now ?? Date.now;
  }

  get size(): number {
    this.prune();
    return this.calls.size;
  }

  /** Drop results past their keep time. */
  private prune(): void {
    const now = this.now();
    for (const [id, c] of this.calls) {
      if (c.readAt !== undefined && now - c.readAt >= this.readKeepMs) this.calls.delete(id);
      else if (c.finishedAt !== undefined && now - c.finishedAt >= this.doneKeepMs) this.calls.delete(id);
    }
  }

  /** Room for one more, dropping the oldest finished results first. False: every slot is a running call. */
  private makeRoom(): boolean {
    this.prune();
    while (this.calls.size >= this.max) {
      let oldest: BackgroundCall | undefined;
      for (const c of this.calls.values()) {
        if (c.finishedAt !== undefined && (!oldest || (c.finishedAt as number) < (oldest.finishedAt as number))) oldest = c;
      }
      if (!oldest) return false;
      this.calls.delete(oldest.callId);
    }
    return true;
  }

  /**
   * Take a running call. `work` must never reject. Undefined when the store is full of running
   * calls or the id is taken: the caller then keeps waiting for the call in the foreground.
   */
  add(callId: string, tool: string, mutating: boolean, startedAt: number, work: Promise<CallResult>, onFinish?: (c: BackgroundCall) => void): BackgroundCall | undefined {
    if (this.calls.has(callId) || !this.makeRoom()) return undefined;
    const call: BackgroundCall = { callId, tool, mutating, startedAt, movedAt: this.now(), done: Promise.resolve() };
    call.done = work.then((r) => {
      call.result = r;
      call.finishedAt = this.now();
      onFinish?.(call);
    });
    this.calls.set(callId, call);
    return call;
  }

  get(callId: string): BackgroundCall | undefined {
    this.prune();
    return this.calls.get(callId);
  }

  /** Mark a finished call's result as read (its keep time starts now, once). */
  markRead(c: BackgroundCall): void {
    if (c.readAt === undefined) c.readAt = this.now();
  }

  list(): BackgroundCall[] {
    this.prune();
    return [...this.calls.values()];
  }
}
