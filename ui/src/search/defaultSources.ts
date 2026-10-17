// The default sources: Chats (the host's chat_search) and Files (the panel's quick_open).
// Each takes what it needs as functions, so the dialog's current filters and selection are read at
// the moment a search runs.

import type { Api } from '../api/client.ts';
import { searchOps, type SearchHit } from '../api/search.ts';
import { absolutePath, baseName, dirName, rowId, type Row, type SearchSource } from './sources.ts';

export interface ChatsDeps {
  api: Api;
  filters: () => { projectId: string | null; archived: boolean };
  currentProjectId: () => string | null;
  open: (hit: SearchHit) => void;
}

export function chatsSource(d: ChatsDeps): SearchSource {
  const ops = searchOps(d.api);
  return {
    id: 'chats',
    minChars: 1,
    timeoutMs: 8_000,
    run: async (query) => {
      const f = d.filters();
      const r = await ops.chats({ query, projectId: f.projectId, archived: f.archived, currentProjectId: d.currentProjectId() });
      return r.hits.map(
        (h): Row => ({
          id: rowId('chats', `${h.chatId}:${h.messageId ?? 'title'}`),
          title: h.title || 'Untitled chat',
          subtitle: h.project,
          snippet: h.snippet || undefined,
          tag: h.titleOnly ? 'title' : undefined,
          open: () => d.open(h),
        }),
      );
    },
  };
}

interface QuickOpenHit {
  path: string;
  score?: number;
}

export interface FilesDeps {
  api: Api;
  /** The chat in view: quick_open searches that chat's folder. Null means no Files group. */
  chatId: () => string | null;
  cwd: () => string | null;
  openFile: (absolutePath: string) => void;
}

export function filesSource(d: FilesDeps): SearchSource {
  return {
    id: 'files',
    minChars: 1,
    timeoutMs: 8_000,
    run: async (query, signal) => {
      const chatId = d.chatId();
      const cwd = d.cwd();
      if (!chatId || !cwd) return [];
      const hits = await d.api<QuickOpenHit[]>('quick_open', { chatId, query });
      if (signal.aborted) return [];
      return hits.map((h): Row => {
        const abs = absolutePath(cwd, h.path);
        return {
          id: rowId('files', abs),
          title: baseName(h.path),
          subtitle: dirName(h.path) || undefined,
          open: () => d.openFile(abs),
        };
      });
    },
  };
}
