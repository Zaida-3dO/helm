// The queue store for the sidebar chip and the panel: the host has no push event for the
// queue, so the store re-reads `queue_list` on a timer while someone is looking, and right
// after every action. Nothing works against a guess: between reads the last answer stands.

import type { QueueSummary } from '../api/generated/index.ts';
import type { QueueAction, QueueOps } from './api.ts';

/** How often the line is re-read while someone watches it. */
export const QUEUE_POLL_MS = 8000;

export class QueueStore {
  private summary: QueueSummary | null = null;
  private listeners = new Set<() => void>();
  private timer: ReturnType<typeof setInterval> | null = null;

  private readonly ops: QueueOps;

  constructor(ops: QueueOps) {
    this.ops = ops;
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    if (this.timer === null) {
      this.refresh();
      this.timer = setInterval(() => this.refresh(), QUEUE_POLL_MS);
    }
    return () => {
      this.listeners.delete(l);
      if (this.listeners.size === 0 && this.timer !== null) {
        clearInterval(this.timer);
        this.timer = null;
      }
    };
  };

  getSnapshot = (): QueueSummary | null => this.summary;

  refresh(): void {
    void this.ops
      .list()
      .then((s) => {
        this.summary = s;
        this.emit();
      })
      .catch(() => {
        /* a failing read keeps the last answer; the next tick tries again */
      });
  }

  /** One move; the host answers with the fresh queue, so the store takes it directly. */
  async act(chatId: string, action: QueueAction): Promise<void> {
    const s = await this.ops.act(chatId, action);
    this.summary = s;
    this.emit();
  }

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }
}
