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

export interface FeedbackThread extends Omit<FeedbackItemRow, 'entryCount' | 'snippet'> {
  entries: FeedbackEntryRow[];
}

export interface FeedbackFilter {
  status?: FeedbackStatus;
  author?: FeedbackAuthorKind;
  text?: string;
}

export function feedbackOps(api: Api) {
  return {
    items: (f: FeedbackFilter = {}) => api<{ items: FeedbackItemRow[] }>('feedback_items', { ...f, limit: 50 }),
    item: (id: number) => api<FeedbackThread>('feedback_item', { id }),
    create: (title: string, text: string) => api<{ id: number }>('feedback_create', { title, text }),
    append: (id: number, text: string) => api<{ id: number }>('feedback_append', { id, text }),
    update: (id: number, change: { status?: FeedbackStatus; boardItem?: string }) => api<{ id: number }>('feedback_update', { id, ...change }),
  };
}

export type FeedbackOps = ReturnType<typeof feedbackOps>;
