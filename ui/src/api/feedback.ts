// Typed wrappers over the feedback-item ops (threads the user and agents file about Helm itself).
// These are not the agents' feature ratings in usage.ts.

import type { Api } from './client.ts';

export type FeedbackStatus = 'open' | 'filed' | 'done' | 'wont_do';
/** Who wrote it: the user in Helm's UI, an agent's own observation, or an agent noting what the user said. */
export type FeedbackAuthorKind = 'human' | 'agent' | 'agent_for_user';

export interface FeedbackAuthor {
  kind: FeedbackAuthorKind;
  /** The chat the agent wrote from; absent for the user. */
  chatId?: string;
}

export interface FeedbackItemRow {
  id: number;
  title: string;
  status: FeedbackStatus;
  boardItem: string | null;
  author: FeedbackAuthor;
  createdAt: number;
  updatedAt: number;
  entryCount: number;
  snippet: string;
}

export interface FeedbackEntryRow {
  id: number;
  text: string;
  author: FeedbackAuthor;
  at: number;
}

/** One thing done to an item after it was filed: a status change, an edit, a delete or a restore. */
export interface FeedbackHistoryRow {
  action: 'status' | 'edited' | 'deleted' | 'restored';
  /** The new status for `status`; what was edited (title, text, title and text) for `edited`. */
  detail: string | null;
  by: FeedbackAuthor;
  at: number;
}

export interface FeedbackThread extends Omit<FeedbackItemRow, 'entryCount' | 'snippet'> {
  entries: FeedbackEntryRow[];
  history: FeedbackHistoryRow[];
  /** Set while the item is deleted: who did it and when. */
  deleted: { by: FeedbackAuthor; at: number } | null;
}

export interface FeedbackFilter {
  status?: FeedbackStatus;
  author?: FeedbackAuthorKind;
  text?: string;
  /** List deleted items instead of live ones. */
  deleted?: boolean;
}

export function feedbackOps(api: Api) {
  return {
    items: (f: FeedbackFilter = {}) => api<{ items: FeedbackItemRow[] }>('feedback_items', { ...f, limit: 50 }),
    item: (id: number) => api<FeedbackThread>('feedback_item', { id }),
    create: (title: string, text: string) => api<{ id: number }>('feedback_create', { title, text }),
    append: (id: number, text: string) => api<{ id: number }>('feedback_append', { id, text }),
    update: (id: number, change: { status?: FeedbackStatus; boardItem?: string }) => api<{ id: number }>('feedback_update', { id, ...change }),
    edit: (id: number, change: { title?: string; text?: string; entryId?: number }) => api<{ id: number }>('feedback_edit', { id, ...change }),
    remove: (id: number) => api<{ id: number }>('feedback_delete', { id }),
    restore: (id: number) => api<{ id: number }>('feedback_restore', { id }),
  };
}

export type FeedbackOps = ReturnType<typeof feedbackOps>;
