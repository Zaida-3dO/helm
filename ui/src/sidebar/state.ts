// The chat's open-item state: which transcript cards are opened and whether the chat's
// sidebar row is expanded. It rides on the chat (one small JSON blob), so every PC viewing
// the chat shows the same, and it survives a reload or the row virtualizing away. The host
// merges a write field by field — a field left out keeps what the chat had — so the
// sidebar's toggle and the transcript's cards never erase each other; last write wins.
// These helpers build the patches and read the state; the same rules live in the chats
// crate's `chat_set_sidebar_state`.

import type { SidebarState } from '../api/types.ts';

export const SIDEBAR_STATE_VERSION = 1;
/** Opened cards kept, at most: opening more folds the ones opened first. */
export const MAX_OPEN_CARDS = 256;

/** A patch that expands or folds this chat's sidebar row, leaving its opened cards as they are. */
export function patchExpanded(expanded: boolean): SidebarState {
  return { v: SIDEBAR_STATE_VERSION, expanded };
}

/** A patch that opens or folds one transcript card (by message id), leaving the row's
 *  expansion as it is. Opening a card moves it to the back of the list; beyond
 *  [`MAX_OPEN_CARDS`] the oldest opened are dropped. */
export function patchCardOpen(prev: SidebarState | null | undefined, rowId: number, open: boolean): SidebarState {
  const cards = (prev?.openCards ?? []).filter((id) => id !== rowId);
  if (open) cards.push(rowId);
  if (cards.length > MAX_OPEN_CARDS) cards.splice(0, cards.length - MAX_OPEN_CARDS);
  return { v: SIDEBAR_STATE_VERSION, openCards: cards };
}

/** Whether a transcript card is open in this state. */
export function cardOpen(state: SidebarState | null | undefined, rowId: number): boolean {
  return state?.openCards?.includes(rowId) === true;
}
