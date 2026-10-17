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

export function panelOps(api: Api) {
  return {
    fileRead: (path: string, chatId?: string | null) =>
      api<FileView>('file_read', chatId ? { chatId, path } : { path }),
    defaultHandler: (path: string) => api<DefaultHandlerResult>('default_handler', { path }),
    openDefault: (path: string) => api<null>('file_open_default', { path }),
    reveal: (path: string) => api<null>('file_reveal', { path }),
    chatFolder: (chatId: string) => api<{ path: string | null }>('chat_folder', { chatId }),
    tabsGet: () => api<unknown>('panel_tabs_get', {}),
    tabsSave: (value: unknown) => api<null>('panel_tabs_save', { value }),
  };
}

export type PanelOps = ReturnType<typeof panelOps>;
