// Quit and close handling.
//
// On `quitting` the UI flushes drafts and pending settings, then tells the host with `quit_ready`.
// The host waits at most 2 s, so a flush that hangs is abandoned after FLUSH_BUDGET_MS rather than
// holding the quit. On `closing` (the window is about to hide) the same flush runs, without the ack.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';

export const FLUSH_BUDGET_MS = 1500;

export type Flusher = () => Promise<void> | void;

export class Flushers {
  private readonly list = new Set<Flusher>();

  add(f: Flusher): () => void {
    this.list.add(f);
    return () => {
      this.list.delete(f);
    };
  }

  /** Run every flusher; resolves when all settle or the budget runs out. */
  async flushAll(budgetMs: number = FLUSH_BUDGET_MS): Promise<void> {
    const all = Promise.allSettled([...this.list].map(async (f) => f()));
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<void>((resolve) => {
      timer = setTimeout(resolve, budgetMs);
    });
    await Promise.race([all.then(() => undefined), timeout]);
    clearTimeout(timer);
  }
}

export function installQuitHandling(bus: EventBus, api: Api, flushers: Flushers): () => void {
  const offQuit = bus.on('quitting', () => {
    void flushers.flushAll().then(() => api('quit_ready'));
  });
  const offClose = bus.on('closing', () => {
    void flushers.flushAll();
  });
  return () => {
    offQuit();
    offClose();
  };
}
