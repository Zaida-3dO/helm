// The chat store: one ThreadView per chat, fed by host events and page reads.
//
// Unwatched threads are evicted when more than MAX_THREADS are held or their text passes
// MAX_BYTES. A watched thread is never evicted; it re-reads its page when it is opened.

import type { EventBus } from '../api/events.ts';
import type { ChatRecord, MessageRow, Submission, ThreadState, TranscriptPage } from '../api/types.ts';
import { perf } from '../perf/perf.ts';
import {
  applyChat,
  applyPage,
  applyQueue,
  applyRows,
  applyThread,
  emptyView,
  type Keep,
  type ThreadView,
} from './thread.ts';

export const MAX_THREADS = 8;
export const MAX_BYTES = 16 * 1024 * 1024;

type Listener = () => void;

function viewBytes(v: ThreadView): number {
  let n = 0;
  for (const r of v.rows) n += (r.text.length + r.thinking.length) * 2;
  return n;
}

/** Feeds the send timeline (perf/marks.ts): a new user row, then the turn's first text. */
function timeRows(chatId: string, rows: MessageRow[], before: ThreadView | undefined): void {
  for (const r of rows) {
    if (r.role === 'user' && r.details?.mode !== 'steer' && !before?.rows.some((h) => h.id === r.id)) perf.userRow(chatId);
    else if (r.role === 'assistant' && r.text) perf.tokenArrived(chatId);
  }
}

export class ChatStore {
  private readonly views = new Map<string, ThreadView>();
  private readonly watched = new Map<string, number>();
  private readonly listeners = new Set<Listener>();
  private chats: ChatRecord[] = [];
  /** Where each chat's reader is; decides which end the row cap trims. */
  private readonly keep = new Map<string, Keep>();

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  private emit(): void {
    for (const l of this.listeners) l();
  }

  private readonly empties = new Map<string, ThreadView>();

  /** Stable per chat between changes, as useSyncExternalStore needs. */
  view(chatId: string): ThreadView {
    const v = this.views.get(chatId);
    if (v) return v;
    let e = this.empties.get(chatId);
    if (!e) {
      e = emptyView(chatId);
      this.empties.set(chatId, e);
    }
    return e;
  }

  chatList(): ChatRecord[] {
    return this.chats;
  }

  setChatList(list: ChatRecord[]): void {
    this.chats = list;
    this.emit();
  }

  setKeep(chatId: string, keep: Keep): void {
    this.keep.set(chatId, keep);
  }

  watch(chatId: string): () => void {
    this.watched.set(chatId, (this.watched.get(chatId) ?? 0) + 1);
    return () => {
      const n = (this.watched.get(chatId) ?? 1) - 1;
      if (n <= 0) this.watched.delete(chatId);
      else this.watched.set(chatId, n);
      this.evict();
    };
  }

  private update(chatId: string, f: (v: ThreadView) => ThreadView): void {
    const prev = this.views.get(chatId) ?? emptyView(chatId);
    const next = f(prev);
    if (next === prev && this.views.has(chatId)) return;
    // Re-insert so Map order is least recently touched first.
    this.views.delete(chatId);
    this.views.set(chatId, next);
    this.empties.delete(chatId);
    this.evict();
    this.emit();
  }

  private evict(): void {
    let bytes = 0;
    for (const v of this.views.values()) bytes += viewBytes(v);
    for (const [id, v] of this.views) {
      if (this.views.size <= MAX_THREADS && bytes <= MAX_BYTES) break;
      if (this.watched.has(id)) continue;
      bytes -= viewBytes(v);
      this.views.delete(id);
    }
  }

  page(page: TranscriptPage, how: 'replace' | 'older' | 'newer'): void {
    this.update(page.chatId, (v) => applyPage(v, page, how));
  }

  chat(chat: ChatRecord): void {
    this.update(chat.id, (v) => applyChat(v, chat));
    const i = this.chats.findIndex((c) => c.id === chat.id);
    if (i >= 0 && this.chats[i]!.revision < chat.revision) {
      // Archived chats and unpromoted side chats leave the list, as chat_list leaves them out.
      this.chats = chat.archived || chat.sideSourceId ? this.chats.filter((c) => c.id !== chat.id) : this.chats.map((c) => (c.id === chat.id ? chat : c));
      this.emit();
    } else if (i < 0 && !chat.archived && !chat.sideSourceId) {
      this.chats = [chat, ...this.chats];
      this.emit();
    }
  }

  thread(thread: ThreadState): void {
    this.update(thread.chatId, (v) => applyThread(v, thread));
  }

  rows(chatId: string, rows: MessageRow[]): void {
    const before = this.views.get(chatId);
    this.update(chatId, (v) => applyRows(v, rows, this.keep.get(chatId) ?? 'newest'));
    timeRows(chatId, rows, before);
  }

  queue(chatId: string, pending: Submission[], revision: number): void {
    this.update(chatId, (v) => applyQueue(v, pending, revision));
  }

  /** The chat is gone (`chat_removed`): forget it. */
  deleted(chatId: string): void {
    this.views.delete(chatId);
    this.empties.delete(chatId);
    this.chats = this.chats.filter((c) => c.id !== chatId);
    this.emit();
  }

  refresh(chatId: string): void {
    this.update(chatId, (v) => ({ ...v, refreshes: v.refreshes + 1 }));
  }

  /** Route the host's events into the store. */
  connect(bus: EventBus): () => void {
    const offs = [
      bus.on('chat', (e) => this.chat(e.chat as ChatRecord)),
      bus.on('thread', (e) => this.thread(e.thread as ThreadState)),
      bus.on('message', (e) => this.rows(e.chatId as string, e.rows as MessageRow[])),
      bus.on('queue', (e) => this.queue(e.chatId as string, e.pending as Submission[], Number(e.revision ?? 0))),
      bus.on('refresh', (e) => this.refresh(e.chatId as string)),
      bus.on('chat_removed', (e) => this.deleted(e.chatId as string)),
    ];
    return () => offs.forEach((o) => o());
  }
}
