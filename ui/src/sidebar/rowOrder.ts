// The sidebar's drawn chat order, live, for the shell's Ctrl+Tab / Ctrl+Shift+Tab.
// The sidebar writes it on every render of its list; the shell reads it when the chord fires and
// selects the chat `stepVisibleChat` returns.

import { stepChat } from './model.ts';

let order: readonly string[] = [];

/** Called by the sidebar with `visibleChatOrder(items)`. */
export function setVisibleOrder(next: readonly string[]): void {
  order = next;
}

/** The chat ids the sidebar draws right now, top to bottom. */
export function visibleOrder(): readonly string[] {
  return order;
}

/** The chat to open for next (1) or previous (-1) from `current`; null when nothing is drawn. */
export function stepVisibleChat(current: string | null, dir: 1 | -1): string | null {
  return stepChat(order, current, dir);
}
