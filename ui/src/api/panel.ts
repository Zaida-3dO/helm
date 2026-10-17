// The side panel's host ops (host/crates/panel). Hand-written shapes for now:
// the panel crate's generated bindings follow once the chats generator tolerates per-crate folders.

import type { Api } from './client.ts';

export type ViewKind = 'text' | 'image' | 'dir' | 'html' | 'binary';

export interface DirEntry {
  name: string;
  kind: 'dir' | 'file' | 'link';
  size?: number;
}

export interface FileView {
  path: string;
  kind: ViewKind;
  size: number;
  text?: string;
  /** Base64 image bytes. */
  bytes?: string;
  mime?: string;
  entries?: DirEntry[];
  truncated: boolean;
  language?: string;
}

export interface Handler {
  name: string;
  appPath: string;
}

export interface HandlerRefusal {
  step: 'path' | 'private' | 'executable' | 'launchType' | 'noHandler' | 'deniedHandler' | 'untrustedHandler';
  reason: string;
}

export interface DefaultHandlerResult {
  handler: Handler | null;
  refusal: HandlerRefusal | null;
}

export interface QuickItem {
  path: string;
  label: string;
  kind: 'dir' | 'file' | 'link';
  score: number;
}

export interface QuickResult {
  mode: 'path' | 'files';
  items: QuickItem[];
  partial: boolean;
}

export type ChangeSection = 'staged' | 'unstaged' | 'conflicts';

export interface ChangeRecord {
  /** Relative to the repository top, forward slashes. */
  path: string;
  status: string;
  from?: string;
  section: ChangeSection;
  partial: boolean;
  added?: number;
  removed?: number;
}

export interface ChangesView {
  repoTop: string;
  branch: string | null;
  records: ChangeRecord[];
  truncated: boolean;
}

export type DiffSide = 'head' | 'staged' | 'unstaged';

export interface DiffView {
  patch: string;
  fullContext: boolean;
  truncated: boolean;
}

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

export interface TurnFile {
  path: string;
  repositoryRoot: string | null;
  repositoryPath: string;
  added: number | null;
  removed: number | null;
  completeCounts: boolean;
}

export interface TurnChanges {
  messageId: number;
  files: TurnFile[];
  truncated: boolean;
}

export function panelOps(api: Api) {
  return {
    fileRead: (path: string, chatId?: string | null) =>
      api<FileView>('file_read', chatId ? { chatId, path } : { path }),
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
    tabsGet: () => api<unknown>('panel_tabs_get', {}),
    tabsSave: (value: unknown) => api<null>('panel_tabs_save', { value }),
  };
}

export type PanelOps = ReturnType<typeof panelOps>;
