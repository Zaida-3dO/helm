// The attention summary on the UI side. The host publishes one summary; this
// store keeps the newest by revision and tells subscribers. Consumers: the sidebar's row marks
// and the in-app toasts, which withdraw a chat's toast once it is no longer in
// `unread`. Nobody recomputes the count.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { AttentionSummary, Completion } from '../api/types.ts';

type Listener = () => void;

export const EMPTY_ATTENTION: AttentionSummary = { badge: 0, busy: 0, needsYou: [], running: [], unread: [], stalled: [], flagged: [], revision: 0 };

export class AttentionStore {
  private summary: AttentionSummary = EMPTY_ATTENTION;
  private readonly listeners = new Set<Listener>();
  private readonly completionListeners = new Set<(c: Completion) => void>();

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  /** Called once per newly announced completion (the `completion` event). */
  onCompletion(l: (c: Completion) => void): () => void {
    this.completionListeners.add(l);
    return () => {
      this.completionListeners.delete(l);
    };
  }

  get = (): AttentionSummary => this.summary;

  /** Apply a summary; an older or equal revision is dropped. */
  apply(s: AttentionSummary): void {
    if (s.revision <= this.summary.revision && this.summary !== EMPTY_ATTENTION) return;
    this.summary = s;
    for (const l of this.listeners) l();
  }

  /** Whether the chat is in the unread group (toasts withdraw when this turns false). */
  isUnread(chatId: string): boolean {
    return this.summary.unread.some((r) => r.chatId === chatId);
  }

  /** Listen for `attention` and `completion` events, then read the current summary once. */
  connect(bus: EventBus, api: Api): () => void {
    const offs = [
      bus.on('attention', (e) => this.apply(e.summary as AttentionSummary)),
      bus.on('completion', (e) => {
        for (const l of this.completionListeners) l(e.completion as Completion);
      }),
    ];
    void api<AttentionSummary>('attention_get', {})
      .then((s) => this.apply(s))
      .catch(() => undefined);
    return () => offs.forEach((o) => o());
  }
}
