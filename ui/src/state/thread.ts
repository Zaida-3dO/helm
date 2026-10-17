// One chat's in-memory view, and the pure rules that update it from pages and host events.
//
// - Stale revisions are dropped: a chat, thread, queue or message row whose revision
//   is not newer than the one held is ignored. `refresh` and `catalogue` carry no revision and
//   always apply. The queue list is replaced whole each time.
// - A thread holds at most MAX_ROWS rows. Rows past the cap are dropped from the end the
//   reader is not looking at, and the paging flag on that side is set again so they can be re-read.

import type { ChatRecord, MessageRow, Submission, ThreadState, TranscriptPage } from '../api/types.ts';

export const MAX_ROWS = 600;

export interface ThreadView {
  chatId: string;
  chat: ChatRecord | null;
  thread: ThreadState | null;
  rows: MessageRow[];
  hasOlder: boolean;
  hasNewer: boolean;
  olderCursor: string | null;
  newerCursor: string | null;
  pending: Submission[];
  /** Revision of the last `queue` event applied; a page read resets it. */
  queueRevision: number;
  /** Bumped by a `refresh` event so the view knows to re-read its page. */
  refreshes: number;
}

export function emptyView(chatId: string): ThreadView {
  return {
    chatId,
    chat: null,
    thread: null,
    rows: [],
    hasOlder: false,
    hasNewer: false,
    olderCursor: null,
    newerCursor: null,
    pending: [],
    queueRevision: 0,
    refreshes: 0,
  };
}

/** Transcript order: (timestamp, sequence, id). */
export function compareRows(a: MessageRow, b: MessageRow): number {
  return a.timestamp - b.timestamp || a.sequence - b.sequence || a.id - b.id;
}

/** Where the reader is, which decides the end the cap trims. */
export type Keep = 'newest' | 'oldest';

/** Upsert rows, dropping any that are not newer than the held copy. Returns the same view if nothing changed. */
export function applyRows(view: ThreadView, incoming: MessageRow[], keep: Keep = 'newest'): ThreadView {
  if (incoming.length === 0) return view;
  const byId = new Map(view.rows.map((r) => [r.id, r]));
  let changed = false;
  for (const row of incoming) {
    if (row.chatId !== view.chatId) continue;
    const held = byId.get(row.id);
    if (held && held.revision >= row.revision) continue;
    byId.set(row.id, row);
    changed = true;
  }
  if (!changed) return view;
  const rows = [...byId.values()].sort(compareRows);
  return capRows({ ...view, rows }, keep);
}

export function capRows(view: ThreadView, keep: Keep): ThreadView {
  if (view.rows.length <= MAX_ROWS) return view;
  if (keep === 'newest') {
    const rows = view.rows.slice(view.rows.length - MAX_ROWS);
    // Assumption until the generated bindings land: a page cursor is the boundary row's id.
    return { ...view, rows, hasOlder: true, olderCursor: String(rows[0]!.id) };
  }
  const rows = view.rows.slice(0, MAX_ROWS);
  return { ...view, rows, hasNewer: true, newerCursor: String(rows[rows.length - 1]!.id) };
}

/** Apply a transcript page. `replace` is a fresh read (open, refresh, or an "around" jump). */
export function applyPage(view: ThreadView, page: TranscriptPage, how: 'replace' | 'older' | 'newer'): ThreadView {
  if (page.chatId !== view.chatId) return view;
  if (how === 'replace') {
    const base: ThreadView = {
      ...view,
      rows: [],
      hasOlder: page.hasOlder,
      hasNewer: page.hasNewer,
      olderCursor: page.olderCursor,
      newerCursor: page.newerCursor,
      pending: page.pending,
      queueRevision: 0,
    };
    const next = applyRows(base, page.messages, 'newest');
    return next === base ? base : next;
  }
  if (how === 'older') {
    const next = applyRows(view, page.messages, 'oldest');
    return { ...next, hasOlder: page.hasOlder, olderCursor: page.olderCursor };
  }
  const next = applyRows(view, page.messages, 'newest');
  return { ...next, hasNewer: page.hasNewer, newerCursor: page.newerCursor };
}

export function applyChat(view: ThreadView, chat: ChatRecord): ThreadView {
  if (chat.id !== view.chatId) return view;
  if (view.chat && view.chat.revision >= chat.revision) return view;
  return { ...view, chat };
}

export function applyThread(view: ThreadView, thread: ThreadState): ThreadView {
  if (thread.chatId !== view.chatId) return view;
  if (view.thread && view.thread.revision >= thread.revision) return view;
  return { ...view, thread };
}

export function applyQueue(view: ThreadView, pending: Submission[], revision: number): ThreadView {
  if (revision <= view.queueRevision) return view;
  return { ...view, pending, queueRevision: revision };
}
