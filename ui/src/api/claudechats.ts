// Typed wrapper over the ops that read Claude Code's own chat records.
// Every call still goes through api(op, args).

import type { Api } from './client.ts';
import type { ChatList, ChatRead, Forked, SearchResults } from './generated/claudechats/index.ts';

export type { ChatList, ChatRead, ChatRow, Line, LineKind, LineRole, SearchHit, SearchResults } from './generated/claudechats/index.ts';

export function claudeChatOps(api: Api) {
  return {
    list: () => api<ChatList>('claude_chat_list', {}),
    search: (query: string) => api<SearchResults>('claude_chat_search', { query }),
    read: (sessionId: string) => api<ChatRead>('claude_chat_read', { sessionId }),
    /** Copies a record into a new Helm chat and answers its id. */
    fork: (sessionId: string) => api<Forked>('claude_chat_fork', { sessionId }),
  };
}

export type ClaudeChatOps = ReturnType<typeof claudeChatOps>;
