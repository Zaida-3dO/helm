// Typed wrapper over the chat search op. Every call still goes through api(op, args).

import type { Api } from './client.ts';
import type { SearchHit, SearchResult } from './generated/search/index.ts';

export type { SearchHit, SearchResult } from './generated/search/index.ts';

export interface SearchQuery {
  query: string;
  projectId?: string | null;
  archived?: boolean;
  /** The project in view, for the ranking boost. */
  currentProjectId?: string | null;
}

export function searchOps(api: Api) {
  return {
    chats: (q: SearchQuery) =>
      api<SearchResult>('chat_search', {
        query: q.query,
        projectId: q.projectId ?? null,
        archived: q.archived ?? false,
        currentProjectId: q.currentProjectId ?? null,
      }),
    recordOpen: (chatId: string) => api<null>('search_open_record', { chatId }),
    clearSignals: () => api<null>('search_signals_clear', {}),
  };
}

export type SearchOps = ReturnType<typeof searchOps>;
export type { SearchHit as Hit };
