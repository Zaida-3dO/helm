// What a `shell_open_chat` event does. The host always sends it when a chat starts another
// (a roll over, an agent's app_open_chat); when it carries `ifViewing`, the window is only to
// be moved while the user is still looking at that chat — otherwise the user gets a notice with
// an Open button, so a chat the app starts never pulls the window away mid-read. Events without
// `ifViewing` (the tray, the pet, a toast) always follow.

export interface OpenEvent {
  chatId?: unknown;
  ifViewing?: unknown;
  title?: unknown;
  messageId?: unknown;
}

export type OpenDecision =
  | { kind: 'select'; chatId: string; messageId?: string }
  | { kind: 'notice'; chatId: string; title: string | null };

/** Decide what to do with the event, given the chat currently on screen. */
export function decideOpen(event: OpenEvent, currentChatId: string | null): OpenDecision | null {
  const chatId = typeof event.chatId === 'string' && event.chatId !== '' ? event.chatId : null;
  if (!chatId) return null;
  const ifViewing = typeof event.ifViewing === 'string' && event.ifViewing !== '' ? event.ifViewing : null;
  if (ifViewing === null || ifViewing === currentChatId) {
    const messageId =
      typeof event.messageId === 'number' && Number.isFinite(event.messageId)
        ? String(event.messageId)
        : typeof event.messageId === 'string' && event.messageId !== ''
          ? event.messageId
          : undefined;
    return { kind: 'select', chatId, messageId };
  }
  const title = typeof event.title === 'string' && event.title.trim() !== '' ? event.title : null;
  return { kind: 'notice', chatId, title };
}
