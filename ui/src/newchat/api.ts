// The host ops the new-chat page uses beyond the chat and project ops.

import type { Api } from '../api/client.ts';
import type { FolderPickArgs, FolderPickResult } from '../api/generated/shell/index.ts';

/** The native "choose a folder" dialog (helm-shell `folder_pick`); null when cancelled. */
export async function pickFolder(api: Api, args: FolderPickArgs = {}): Promise<string | null> {
  const r = await api<FolderPickResult>('folder_pick', args);
  return r.path;
}
