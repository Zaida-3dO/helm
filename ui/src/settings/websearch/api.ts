// The web search ops (host/crates/webtools) that Settings → Web search uses. The Brave key is
// write-only: no op returns it, only whether one is set.

import type { Api } from '../../api/client.ts';

export interface WebSearchView {
  hasKey: boolean;
  /** Claude chats are offered the search tool too (local-model chats always are, with a key). */
  offerInClaudeChats: boolean;
  /** The settings file "Import from OpenClaw" will read: the last one used, else the default. */
  importPath: string;
  defaultImportPath: string;
}

/** One search's outcome: OK, or Brave's refusal in plain words. */
export interface WebSearchTest {
  ok: boolean;
  message: string;
}

export function webSearchOps(api: Api) {
  return {
    get: () => api<WebSearchView>('web_search_get', {}),
    save: (offerInClaudeChats: boolean) => api<WebSearchView>('web_search_save', { offerInClaudeChats }),
    setKey: (key: string) => api<WebSearchView>('web_search_set_key', { key }),
    forgetKey: () => api<WebSearchView>('web_search_forget_key', {}),
    test: () => api<WebSearchTest>('web_search_test', {}),
    importKey: (path: string) => api<{ imported: true }>('web_search_import', { path }),
  };
}
