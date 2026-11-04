// The new-chat area's public surface. Other areas start a chat or add a project only through these.
//
//   openNewChat()                     Ctrl+N, the sidebar's New chat, the empty state
//   openNewChat({ projectId })        a project's "+" or its menu's New chat
//   openNewChat({ folder })           start in a given folder
//   openAddProject()                  Ctrl+O, the empty state's "Open a project": the New project
//                                     form (a new board project with its folder)
//   openNewProject({ noChat })        the same form; Settings opens it without starting a chat
//   openAddFolder()                   Settings → Projects → Folders: add a folder chats can run in
//   openPrimaryFolder(projectId)      a board's "Set folder": the folder its new chats start in
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

/** Open the New project form (Ctrl+O): a new board project, then a chat in it. */
export function openAddProject(): void {
  openNewProject();
}

/** Open the New project form. With `noChat` it only makes the project (Settings). */
export function openNewProject(opts: { title?: string; noChat?: boolean } = {}): void {
  newChat.openDialog({ kind: 'new-project', ...opts });
}

/** Open the Add folder dialog: an existing folder or a new folder by name, for chats to run in. */
export function openAddFolder(): void {
  newChat.openDialog({ kind: 'add-project' });
}

/** Open the Primary folder dialog for a board ("Set folder"). */
export function openPrimaryFolder(projectId: string): void {
  newChat.openDialog({ kind: 'primary-folder', projectId });
}

/** Subscribe to the page's place (prewarm hook). Returns the unsubscribe. */
export function onNewChatReady(f: ReadyListener): () => void {
  return newChat.onReady(f);
}
