// The new-chat area's public surface. Other areas start a chat or add a project only through these.
//
//   openNewChat()                     Ctrl+N, the sidebar's New chat, the empty state
//   openNewChat({ projectId })        a project's "+" or its menu's New chat
//   openNewChat({ folder })           start in a given folder
//   openAddProject()                  Ctrl+O, "Open a project" (lands with PR N3)
//   onNewChatReady(f)                 called with the page's project and folder whenever it opens
//                                     or they change: the hook for warming a worker early
//
// Nothing is created until the first send: an untouched new chat leaves nothing
// behind.

import { newChat, type NewChatOptions, type ReadyListener } from './controller.ts';

export type { NewChatOptions } from './controller.ts';
export { NewChatHost } from './NewChatPage.tsx';

/** Open the new-chat page, optionally in a project or folder. Reuses an open draft. */
export function openNewChat(opts?: NewChatOptions): void {
  newChat.show(opts ?? {});
}

/** Open the Add project dialog (Ctrl+O). Until PR N3 lands it opens the new-chat page. */
export function openAddProject(): void {
  newChat.show({});
}

/** Subscribe to the page's place (prewarm hook). Returns the unsubscribe. */
export function onNewChatReady(f: ReadyListener): () => void {
  return newChat.onReady(f);
}
