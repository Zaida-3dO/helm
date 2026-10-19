// The new-chat area's public surface. Other areas start a chat or add a project only through these.
//
//   openNewChat()                     Ctrl+N, the sidebar's New chat, the empty state
//   openNewChat({ projectId })        a project's "+" or its menu's New chat
//   openNewChat({ folder })           start in a given folder
//   openAddProject()                  Ctrl+O, the empty state's "Open a project"
//   primaryFolderMenuItem(project)    the project menu's "Primary folder…" item, for the sidebar
//                                     to place in its menu
//   onNewChatReady(f)                 called with the page's project and folder whenever it opens
//                                     or they change: the hook for warming a worker early
//
// Nothing is created until the first send: an untouched new chat leaves nothing
// behind.

import { newChat, type NewChatOptions, type ReadyListener } from './controller.ts';

export type { NewChatOptions } from './controller.ts';
export { NewChatHost } from './NewChatPage.tsx';
export { primaryFolderMenuItem } from './menu.ts';

/** Open the new-chat page, optionally in a project or folder. Reuses an open draft. */
export function openNewChat(opts?: NewChatOptions): void {
  newChat.show(opts ?? {});
}

/** Open the Add project dialog (Ctrl+O): an existing folder or a new folder by name. */
export function openAddProject(): void {
  newChat.openDialog({ kind: 'add-project' });
}

/** Subscribe to the page's place (prewarm hook). Returns the unsubscribe. */
export function onNewChatReady(f: ReadyListener): () => void {
  return newChat.onReady(f);
}
