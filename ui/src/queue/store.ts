// The queue store for the sidebar chip and the panel: the host has no push event for the
// queue, so the store re-reads `queue_list` on a timer while someone is looking, and right
// after every action. Nothing works against a guess: between reads the last answer stands.
// The build queue store below needs no timer: the host publishes `build_queue_changed`
// whenever the line moves, and every action answers with the fresh list.

import type { BuildQueueAddArgs, BuildQueueEntry, QueueSummary } from '../api/generated/index.ts';
import type { BuildQueueOps, QueueAction, QueueOps } from './api.ts';

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
  async act(chatId: string, action: QueueAction, beforeChatId?: string | null): Promise<void> {
    const s = await this.ops.act(chatId, action, beforeChatId);
    this.summary = s;
    this.emit();
  }

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }
}

/** The build queue's list: the panel mounts this while it is open, the first subscriber
 * triggers the first read, and `onHostChange` keeps it current while it stands. Every
 * action answers with the fresh list, so a refusal (or a lost race) leaves the last
 * honest answer on screen rather than a guess. */
export class BuildQueueStore {
  private entries: BuildQueueEntry[] | null = null;
  private listeners = new Set<() => void>();

  private readonly ops: BuildQueueOps;

  constructor(ops: BuildQueueOps) {
    this.ops = ops;
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    if (this.listeners.size === 1) this.refresh();
    return () => {
      this.listeners.delete(l);
    };
  };

  getSnapshot = (): BuildQueueEntry[] | null => this.entries;

  refresh(): void {
    void this.ops
      .list()
      .then((entries) => {
        this.entries = entries;
        this.emit();
      })
      .catch(() => {
        /* a failing read keeps the last answer; the next event or action tries again */
      });
  }

  /** Join the back of the line; the caller must have checked the title and the brief-or-item. */
  async add(args: BuildQueueAddArgs): Promise<void> {
    this.set(await this.ops.add(args));
  }

  async move(id: string, beforeId?: string | null): Promise<void> {
    this.set(await this.ops.move(id, beforeId));
  }

  async pause(id: string): Promise<void> {
    this.set(await this.ops.pause(id));
  }

  async resume(id: string): Promise<void> {
    this.set(await this.ops.resume(id));
  }

  async remove(id: string): Promise<void> {
    this.set(await this.ops.remove(id));
  }

  private set(entries: BuildQueueEntry[]): void {
    this.entries = entries;
    this.emit();
  }

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }
}
