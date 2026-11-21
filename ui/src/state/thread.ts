// One chat's in-memory view, and the pure rules that update it from pages and host events.
//
// - Stale revisions are dropped: a chat, thread, in-flight list or message row whose revision
//   is not newer than the one held is ignored. `refresh` and `catalogue` carry no revision and
//   always apply. The in-flight list is replaced whole each time.
// - A thread holds at most MAX_ROWS rows. Rows past the cap are dropped from the end the
//   reader is not looking at, and the paging flag on that side is set again so they can be re-read.

import type { ChatRecord, MessageRow, SessionMark, Submission, ThreadState, TranscriptPage } from '../api/types.ts';

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
  /** Revision of the last `pending` event applied; a page read resets it. */
  pendingRevision: number;
  /** When the newest run ended (null while it runs or when there is none), so the settled line can end there. */
  lastRunEndedAt: number | null;
  /** Bumped by a `refresh` event so the view knows to re-read its page. */
  refreshes: number;
  /**
   * The sessions whose rows may sit in this view: the chat itself, plus, when the page was read
   * across a roll-over stack, every earlier or later session the pages carried.
   */
  sessions: string[];
  /**
   * Where the loaded rows meet a session boundary (one entry per older session), merged across
   * pages; the transcript draws a divider from these.
   */
  sessionMarks: SessionMark[];
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
    pendingRevision: 0,
    lastRunEndedAt: null,
    refreshes: 0,
    sessions: [chatId],
    sessionMarks: [],
  };
}

/** The end of the newest run in a page's run list; null when that run has not ended. */
function endOfLastRun(runs: TranscriptPage['runs']): number | null {
  return runs.at(-1)?.endedAt ?? null;
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
    // Only this chat's sessions: for a stack window, its archived earlier and later sessions too.
    if (!view.sessions.includes(row.chatId)) continue;
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

/** The sessions a page adds to the set whose rows the view accepts (chat ids are few). */
function mergeSessions(view: ThreadView, page: TranscriptPage): string[] {
  let out: string[] | null = null;
  for (const r of page.messages) {
    if (view.sessions.includes(r.chatId) || out?.includes(r.chatId)) continue;
    out = [...(out ?? view.sessions), r.chatId];
  }
  return out ?? view.sessions;
}

/**
 * One boundary mark per older session. Pages can report the same boundary twice (one side in each
 * page); the copy with fuller anchors wins.
 */
function mergeMarks(existing: SessionMark[], page: TranscriptPage): SessionMark[] {
  const incoming = page.sessionMarks ?? [];
  if (incoming.length === 0) return existing;
  const bySession = new Map(existing.map((m) => [m.sessionId, m]));
  const anchors = (m: SessionMark) => (m.aboveMessageId == null ? 0 : 1) + (m.belowMessageId == null ? 0 : 1);
  for (const m of incoming) {
    const held = bySession.get(m.sessionId);
    if (!held || anchors(m) > anchors(held)) bySession.set(m.sessionId, m);
  }
  return [...bySession.values()];
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
      pendingRevision: 0,
      lastRunEndedAt: endOfLastRun(page.runs),
      sessions: mergeSessions({ ...view, sessions: [page.chatId] }, page),
      sessionMarks: mergeMarks([], page),
    };
    const next = applyRows(base, page.messages, 'newest');
    return next === base ? base : next;
  }
  // The session set grows before the rows are applied: a page that runs past a boundary carries
  // rows of a session the view has not seen.
  const sessions = mergeSessions(view, page);
  const sessionMarks = mergeMarks(view.sessionMarks, page);
  if (how === 'older') {
    const next = applyRows({ ...view, sessions }, page.messages, 'oldest');
    return { ...next, hasOlder: page.hasOlder, olderCursor: page.olderCursor, sessions, sessionMarks };
  }
  const next = applyRows({ ...view, sessions }, page.messages, 'newest');
  return { ...next, hasNewer: page.hasNewer, newerCursor: page.newerCursor, sessions, sessionMarks, lastRunEndedAt: endOfLastRun(page.runs) };
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

export function applyPending(view: ThreadView, pending: Submission[], revision: number): ThreadView {
  if (revision <= view.pendingRevision) return view;
  return { ...view, pending, pendingRevision: revision };
}
