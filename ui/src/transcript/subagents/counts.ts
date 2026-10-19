// Running subagents per chat, for the sidebar's "working" indicator on an idle chat. Fed straight from the event bus's thread events, so it does not depend on which chats'
// threads the chat store happens to hold. The sidebar renders it: `useSubagentsRunning(chatId)`.

import { useSyncExternalStore } from 'react';
import type { EventBus } from '../../api/events.ts';
import type { ThreadState } from '../../api/types.ts';
import { useShell } from '../../shell/context.ts';

type Listener = () => void;

export class SubagentCounts {
  private readonly counts = new Map<string, { n: number; revision: number }>();
  private readonly listeners = new Set<Listener>();

  /** A thread event: the newest revision per chat wins. */
  apply(thread: Pick<ThreadState, 'chatId' | 'revision' | 'subagentsRunning'>): void {
    const prev = this.counts.get(thread.chatId);
    if (prev && prev.revision > thread.revision) return;
    const n = thread.subagentsRunning ?? 0;
    this.counts.set(thread.chatId, { n, revision: thread.revision });
    if ((prev?.n ?? 0) !== n) for (const l of [...this.listeners]) l();
  }

  /** The chat is gone. */
  forget(chatId: string): void {
    if (this.counts.delete(chatId)) for (const l of [...this.listeners]) l();
  }

  running(chatId: string): number {
    return this.counts.get(chatId)?.n ?? 0;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  connect(bus: EventBus): () => void {
    const offs = [
      bus.on('thread', (e) => this.apply(e.thread as ThreadState)),
      bus.on('chat_removed', (e) => this.forget(e.chatId as string)),
    ];
    return () => offs.forEach((o) => o());
  }
}

const perBus = new WeakMap<EventBus, SubagentCounts>();

/** One tracker per bus, connected on first use and kept for the app's life. */
export function countsFor(bus: EventBus): SubagentCounts {
  let c = perBus.get(bus);
  if (!c) {
    c = new SubagentCounts();
    c.connect(bus);
    perBus.set(bus, c);
  }
  return c;
}

/** How many subagents of this chat are running now (0 when none, or not known yet). */
export function useSubagentsRunning(chatId: string): number {
  const counts = countsFor(useShell().bus);
  return useSyncExternalStore(counts.subscribe, () => counts.running(chatId));
}
