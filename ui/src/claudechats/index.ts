// The read-only viewer for one of Claude Code's own chats. Like the other dialogs it opens
// through the shell's one dialog id; the record it shows travels in this small store.

import { useSyncExternalStore } from 'react';

export const CLAUDE_CHAT_DIALOG = 'claude-chat';

let pending: string | null = null;
const listeners = new Set<() => void>();

function notify(): void {
  for (const l of [...listeners]) l();
}

/** Which record the viewer should show (cleared by the viewer when it closes). */
export function setClaudeChatSession(id: string | null): void {
  if (pending === id) return;
  pending = id;
  notify();
}

/** Asks the shell to open the dialog and remembers the record until the viewer reads it. */
export function openClaudeChatViewer(openDialog: (id: string) => void, sessionId: string): void {
  setClaudeChatSession(sessionId);
  openDialog(CLAUDE_CHAT_DIALOG);
}

/** The record id, re-rendering on change. */
export function usePendingClaudeChatSession(): string | null {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => pending,
  );
}

export { ClaudeChatViewer } from './ClaudeChatViewer.tsx';
