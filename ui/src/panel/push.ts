// A chat that shows the user a file or a page (app_open_panel) pushes it into its OWN chat's
// panel, even while another chat is on screen: nothing here may change what the shown chat's
// panel looks like. A refused push for the chat on screen falls back (the modal viewer, a
// notice); a refused push for any other chat stays silent — its card keeps an Open button.

import { displayPath, pathKey } from './viewer/paths.ts';

const NO_ROOM = 'The side panel has no room for another tab.';

export interface PanelOpenDeps {
  /** Opens a document tab in the named chat's set; false when the panel has no room. */
  openTab: (kind: string, key: string, data: unknown, opts: { intent: 'user'; chatId: string }) => boolean;
  /** Opens a pinned page in the named chat's set; false when there is no room. */
  openPage: (path: string, chatId: string) => boolean;
  /** The modal viewer, for a refused file open in the chat on screen. */
  openModal: (path: string, opts: { line?: number; chatId: string }) => void;
  notice: (text: string) => void;
  /** The chat on screen. */
  current: () => string | null;
}

/** Applies one `panel_open` event from the host. Returns true when a tab was opened. */
export function applyPanelOpen(e: Record<string, unknown>, deps: PanelOpenDeps): boolean {
  const chatId = typeof e.chatId === 'string' && e.chatId !== '' ? e.chatId : null;
  if (!chatId) return false;
  const shown = chatId === deps.current();
  if (e.doc === 'file') {
    if (typeof e.path !== 'string' || e.path === '') return false;
    const path = displayPath(e.path);
    const line = typeof e.line === 'number' && Number.isInteger(e.line) && e.line >= 1 ? e.line : undefined;
    if (deps.openTab('file', pathKey(path), line === undefined ? { path } : { path, line }, { intent: 'user', chatId })) {
      return true;
    }
    if (shown) deps.openModal(path, line === undefined ? { chatId } : { line, chatId });
    return false;
  }
  if (e.doc === 'page') {
    if (typeof e.path !== 'string' || e.path === '') return false;
    if (deps.openPage(e.path, chatId)) return true;
    if (shown) deps.notice(NO_ROOM);
    return false;
  }
  return false;
}
