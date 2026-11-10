// The chat menu's "Move to profile": the choices and the move.

import type { ProfilesSnapshot, ProfilesStore } from './store.ts';
import { profileOfChat, type LabelledRow } from './model.ts';

export interface ChatProfileChoice {
  id: string;
  name: string;
  /** The profile the chat belongs to now. */
  current: boolean;
}

/** Every profile as a choice for one chat; `[]` while there is nothing to choose between. */
export function profileChoices(snap: ProfilesSnapshot, chatId: string, rows: readonly LabelledRow[]): ChatProfileChoice[] {
  const view = snap.view;
  if (!view || view.homeId === null || view.profiles.length < 2) return [];
  const now = profileOfChat(chatId, new Map(rows.map((r) => [r.chatId, r])), snap.labels, view.homeId);
  return [...view.profiles]
    .sort((a, b) => Number(b.isHome) - Number(a.isHome))
    .map((p) => ({ id: p.id, name: p.name, current: p.id === now }));
}

/** Label the chat with a profile; a refusal goes to `onError` in the host's words. */
export function moveChatToProfile(store: ProfilesStore, chatId: string, profileId: string, onError: (text: string) => void): void {
  store.ops
    .assign([chatId], profileId)
    .then((v) => store.apply(v))
    .catch((e: unknown) => onError(String((e as { message?: unknown })?.message ?? e)));
}
