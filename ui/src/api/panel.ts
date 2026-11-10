// The side panel's host ops (host/crates/panel). The shapes the host sends are generated from its
// Rust types into ./generated/panel/; the few that only the renderer builds or narrows are written here.

import type { Api } from './client.ts';
import type {
  ArchiveEntry,
  ArchiveView,
  ChangeRecord,
  ChangesView,
  DiffView,
  DirEntry,
  FileStamp,
  FileView,
  Handler,
  QuickItem,
  QuickResult,
  Refusal,
  Section,
  TurnChanges,
  TurnDiff,
  TurnFile,
  ViewKind,
} from './generated/panel/index.ts';

export type { ArchiveEntry, ArchiveView, ChangeRecord, ChangesView, DiffView, DirEntry, FileStamp, FileView, Handler, QuickItem, QuickResult, TurnChanges, TurnDiff, TurnFile, ViewKind };

export type HandlerRefusal = Refusal;

export interface DefaultHandlerResult {
  handler: Handler | null;
  refusal: HandlerRefusal | null;
}

export type ChangeSection = Section;

export type DiffSide = 'head' | 'staged' | 'unstaged';

export interface ReviewComment {
  id: string;
  path: string;
  diffSide: DiffSide;
  side: 'left' | 'right';
  startLine: number;
  endLine: number;
  quote: string;
  body: string;
  createdAt: number;
}

export interface NewReviewComment {
  chatId: string;
  id: string;
  path: string;
  diffSide: DiffSide;
  side: 'left' | 'right';
  startLine: number;
  endLine: number;
  body: string;
  fingerprint: string;
}

/** The side panel context item (host context.rs `Item`). */
export type ContextItem =
  | { kind: 'file'; path: string; line?: number; selection?: { startLine: number; endLine: number; text: string } }
  | { kind: 'folder'; path: string }
  | { kind: 'diff'; repoTop: string; path: string; side: DiffSide; startLine: number; endLine: number; rows: Array<{ sign: '+' | '-' | ' '; text: string }> }
  | { kind: 'reviewComment'; id: string; path: string; side: 'left' | 'right'; startLine: number; endLine: number; quote: string; body: string };

export function panelOps(api: Api) {
  return {
    fileRead: (path: string, chatId?: string | null) =>
      api<FileView>('file_read', chatId ? { chatId, path } : { path }),
    /** Change stamps for up to 8 open files, in order; null for one that is gone or not readable. */
    fileStat: (paths: string[]) => api<Array<FileStamp | null>>('file_stat', { paths }),
    archiveList: (path: string, chatId?: string | null) =>
      api<ArchiveView>('archive_list', chatId ? { chatId, path } : { path }),
    defaultHandler: (path: string) => api<DefaultHandlerResult>('default_handler', { path }),
    openDefault: (path: string) => api<null>('file_open_default', { path }),
    reveal: (path: string) => api<null>('file_reveal', { path }),
    chatFolder: (chatId: string) => api<{ path: string | null }>('chat_folder', { chatId }),
    pathResolve: (path: string, chatId?: string | null) =>
      api<{ path: string; kind: 'file' | 'dir' }>('path_resolve', chatId ? { chatId, path } : { path }),
    quickOpen: (query: string, chatId?: string | null, reload = false) =>
      api<QuickResult>('quick_open', { ...(chatId ? { chatId } : {}), query, ...(reload ? { reload } : {}) }),
    changes: (chatId: string) => api<ChangesView>('changes', { chatId }),
    diff: (chatId: string, path: string, side: DiffSide, from?: string) =>
      api<DiffView>('diff', from ? { chatId, path, side, from } : { chatId, path, side }),
    stage: (chatId: string, path: string) => api<{ changes: ChangesView; noop: boolean }>('changes_stage', { chatId, path }),
    unstage: (chatId: string, path: string, from?: string) =>
      api<{ changes: ChangesView; noop: boolean }>('changes_unstage', from ? { chatId, path, from } : { chatId, path }),
    contextGet: (chatId: string) => api<{ on: boolean }>('panel_context_get', { chatId }),
    contextSet: (chatId: string, on: boolean) => api<{ on: boolean }>('panel_context_set', { chatId, on }),
    commentAdd: (c: NewReviewComment) => api<ReviewComment>('review_comment_add', { ...c }),
    commentList: (chatId: string) => api<ReviewComment[]>('review_comment_list', { chatId }),
    commentRemove: (chatId: string, id: string) => api<ReviewComment[]>('review_comment_remove', { chatId, id }),
    commentClear: (chatId: string) => api<ReviewComment[]>('review_comment_clear', { chatId }),
    reviewTurns: (chatId: string, messageIds: number[]) => api<TurnChanges[]>('review_turns', { chatId, messageIds }),
    turnDiff: (chatId: string, messageId: number, path: string) =>
      api<TurnDiff>('review_turn_diff', { chatId, messageId, path }),
    tabsGet: () => api<unknown>('panel_tabs_get', {}),
    tabsSave: (value: unknown) => api<null>('panel_tabs_save', { value }),
  };
}

export type PanelOps = ReturnType<typeof panelOps>;
