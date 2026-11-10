// The chat menu's "Move to profile" and "Share with": the choices and the actions.

import type { ProfilesSnapshot, ProfilesStore } from './store.ts';
import { sharedWithOf, type IdentityRow } from './model.ts';

export interface ChatProfileChoice {
  id: string;
  name: string;
  /** The profile the chat belongs to now. */
  current: boolean;
}

export interface ChatShareChoice {
  id: string;
  name: string;
  /** The chat is shared with this profile now. */
  shared: boolean;
}

/**
 * Every profile as a choice for one chat; `[]` while there is nothing to choose between. The chat's
 * profile is the one its row names; a row that does not say belongs to the profile showing.
 */
export function profileChoices(snap: ProfilesSnapshot, chatId: string, rows: readonly IdentityRow[]): ChatProfileChoice[] {
  const view = snap.view;
  if (!view || view.homeId === null || view.profiles.length < 2) return [];
  const now = rows.find((r) => r.chatId === chatId)?.profileId ?? view.activeId ?? view.homeId;
  return [...view.profiles]
    .sort((a, b) => Number(b.isHome) - Number(a.isHome))
    .map((p) => ({ id: p.id, name: p.name, current: p.id === now }));
}

/** The other profiles a chat can be shared with (the chat's own profile is not offered), checked where it is. */
export function shareChoices(snap: ProfilesSnapshot, chatId: string, rows: readonly IdentityRow[]): ChatShareChoice[] {
  const view = snap.view;
  if (!view || view.homeId === null || view.profiles.length < 2) return [];
  const row = rows.find((r) => r.chatId === chatId);
  const owner = row?.profileId ?? view.activeId ?? view.homeId;
  const shared = sharedWithOf(row, snap.shared.get(chatId));
  return [...view.profiles]
    .filter((p) => p.id !== owner)
    .sort((a, b) => Number(b.isHome) - Number(a.isHome))
    .map((p) => ({ id: p.id, name: p.name, shared: shared.has(p.id) }));
}

/** Label the chat with a profile; a refusal goes to `onError` in the host's words. */
export function moveChatToProfile(store: ProfilesStore, chatId: string, profileId: string, onError: (text: string) => void): void {
  store.ops
    .assign([chatId], profileId)
    .then((v) => store.apply(v))
    .catch((e: unknown) => onError(String((e as { message?: unknown })?.message ?? e)));
}

/** Share the chat with a profile, or stop sharing; a refusal goes to `onError`. */
export function shareChatWith(store: ProfilesStore, chatId: string, choices: readonly ChatShareChoice[], profileId: string, onError: (text: string) => void): void {
  const now = new Set(choices.filter((c) => c.shared).map((c) => c.id));
  store.share(chatId, profileId, !now.has(profileId), now).catch((e: unknown) => onError(String((e as { message?: unknown })?.message ?? e)));
}
