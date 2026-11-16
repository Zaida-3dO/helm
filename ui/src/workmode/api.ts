// Typed wrappers over the working mode's ops. They are the user's own: no agent tool reads or
// writes the mode, so every call comes from this UI (the top-bar chip or Settings).

import type { Api } from '../api/client.ts';
import type { WorkMode, WorkModeView } from '../api/generated/gatekeeper/index.ts';

export interface WorkModeSet {
  scope: 'app' | 'board';
  boardId?: string;
  /** `run` or `check_in`; null clears a board's own mode, giving it back to the app. */
  mode: WorkMode | null;
}

export function workmodeOps(api: Api) {
  return {
    /** The mode for the given chat, or the app's own when no chat is named. */
    get: (chatId?: string) => api<WorkModeView>('workmode_get', chatId ? { chatId } : {}),
    set: (s: WorkModeSet) => api<WorkModeView>('workmode_set', s),
  };
}

export type WorkmodeOps = ReturnType<typeof workmodeOps>;
