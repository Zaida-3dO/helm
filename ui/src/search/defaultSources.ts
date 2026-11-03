// The default sources: Chats (the host's chat_search) and Files (the panel's quick_open).
// Each takes what it needs as functions, so the dialog's current filters and selection are read at
// the moment a search runs.

import type { Api } from '../api/client.ts';
import { claudeChatOps, type SearchHit as ClaudeHit } from '../api/claudechats.ts';
import type { QuickResult } from '../api/panel.ts';
import { searchOps, type SearchHit } from '../api/search.ts';
import { absolutePath, baseName, dirName, rowId, shortFolder, type Row, type SearchSource } from './sources.ts';

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

export interface ClaudeChatsDeps {
  api: Api;
  /** Opens the read-only viewer for one record. */
  open: (hit: ClaudeHit) => void;
}

/** Chats from Claude Code's own records, outside Helm. */
export function claudeChatsSource(d: ClaudeChatsDeps): SearchSource {
  const ops = claudeChatOps(d.api);
  return {
    id: 'claudechats',
    // The record scan is heavier than the chat index: two characters keeps it off single letters.
    minChars: 2,
    timeoutMs: 8_000,
    run: async (query) => {
      const r = await ops.search(query);
      return r.hits.map(
        (h): Row => ({
          id: rowId('claudechats', h.sessionId),
          title: h.title || 'Untitled chat',
          subtitle: h.folder || undefined,
          snippet: h.snippet || undefined,
          tag: h.titleOnly ? 'title' : undefined,
          open: () => d.open(h),
        }),
      );
    },
  };
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
      const res = await d.api<QuickResult>('quick_open', { chatId, query });
      if (signal.aborted) return [];
      return res.items.map((h): Row => {
        const abs = absolutePath(cwd, h.path);
        // In file-search mode the label is the path inside the folder; in path mode it is just the name.
        const shown = res.mode === 'files' ? h.label || h.path : h.path;
        return {
          id: rowId('files', abs),
          title: baseName(shown),
          // The row's folder is shown short: inside the chat's folder it is relative to it.
          subtitle: shortFolder(dirName(abs), cwd) || undefined,
          // A folder opens in the panel's viewer, which lists it; the tag tells it apart from a file.
          tag: h.kind === 'dir' ? 'folder' : undefined,
          open: () => d.openFile(abs),
        };
      });
    },
  };
}
