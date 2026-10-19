// The host ops the new-chat page uses beyond the chat and project ops.

import type { Api } from '../api/client.ts';
import type { FolderPickArgs, FolderPickResult } from '../api/generated/shell/index.ts';

/**
 * Discard a chat only if it is an untouched new chat (helm-chats `chat_discard_untouched`); a chat
 * with anything in it is left alone. True when it was removed.
 */
export async function discardUntouched(api: Api, chatId: string): Promise<boolean> {
  const r = await api<{ discarded: boolean }>('chat_discard_untouched', { chatId });
  return r.discarded;
}

/** The native "choose a folder" dialog (helm-shell `folder_pick`); null when cancelled. */
export async function pickFolder(api: Api, args: FolderPickArgs = {}): Promise<string | null> {
  const r = await api<FolderPickResult>('folder_pick', args);
  return r.path;
}
