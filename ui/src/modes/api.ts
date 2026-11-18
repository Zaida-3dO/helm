// The mode ops: a chat's modes (the chips by the composer) and the mode files Settings edits.
// A mode is a Markdown file in a knowledge layer that a chat can switch on; the chat's own list
// of which are on lives in the host.

import type { Api } from '../api/client.ts';
import type { ModeFile } from '../api/generated/skills/index.ts';

export type { ModeFile } from '../api/generated/skills/index.ts';

/** A mode as the picker lists it. */
export interface ModeBrief {
  name: string;
  description: string;
  layer: string;
}

/** A mode that is on in a chat, and what switched it on: `project`, `user`, `agent` or `carried`. */
export interface ActiveMode extends ModeBrief {
  source: string;
}

/** A chat's modes: the ones on, the ones switched off again, and the rest that could be added. */
export interface ChatModes {
  available: ModeBrief[];
  active: ActiveMode[];
  off: ModeBrief[];
}

export interface ChatModesChange {
  add?: string[];
  remove?: string[];
}

/** What a mode file holds, bar its name and the layer it is saved in. */
export interface ModeDraft {
  description: string;
  triggers: string[];
  tools: string[];
  mcp: string[];
  skills: string[];
  rules: string[];
  builtins: string[];
  model: string | null;
  body: string;
}

export interface ModesList {
  layer: string | null;
  folder: string | null;
  modes: ModeFile[];
}

export function modeOps(api: Api) {
  return {
    chatGet: (chatId: string) => api<ChatModes>('chat_modes_get', { chatId }),
    chatSet: (chatId: string, change: ChatModesChange) => api<ChatModes>('chat_modes_set', { chatId, ...change }),
    /** With a layer: that layer's own files. Without: the modes a chat of this person gets, merged. */
    list: (layer?: string) => api<ModesList>('modes_list', layer ? { layer } : {}),
    /** A changed file sends the `expectedRevision` it was read at; a stale one is a `conflict`. */
    save: (layer: string, name: string, draft: ModeDraft, expectedRevision?: string) =>
      api<{ mode: ModeFile }>('modes_save', { layer, name, ...draft, ...(expectedRevision ? { expectedRevision } : {}) }),
    remove: (layer: string, name: string) => api<{ removed: boolean }>('modes_remove', { layer, name }),
  };
}

export type ModeOps = ReturnType<typeof modeOps>;
