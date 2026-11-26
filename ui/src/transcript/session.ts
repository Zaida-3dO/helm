// A chat that rolled over is one window over several sessions. Each row belongs to the session
// that wrote it, and the host keeps a row's pictures, turn changes, fork point and the rest under
// that session's id. These are the rules for telling a row of the window's own session from a row
// of an earlier one.

import type { SlotContext } from './slots.ts';

/** True for a row an earlier session wrote: its session is archived, so it cannot be edited or resent. */
export function isEarlierSession(row: { chatId: string }, windowChatId: string): boolean {
  return row.chatId !== windowChatId;
}

/**
 * The session a selection anchors in: the owner of the anchor row, or the window's chat when the
 * row is not loaded. A side chat forks from this session, since only it holds the row.
 */
export function anchorSession(rows: readonly { id: number; chatId: string }[], anchorId: number, windowChatId: string): string {
  return rows.find((r) => r.id === anchorId)?.chatId ?? windowChatId;
}

/** What the slot context of a row is built from: the window's chat and what the transcript is doing now. */
export interface SlotInputs {
  chatId: string;
  latestAssistantId: number | null;
  running: boolean;
  interactionOpen: boolean;
  remoteDevice?: string;
}

/**
 * The context a row's actions and footers see. `chatId` is the chat in the window, where a message
 * an action sends goes; `sessionId` is the row's own session, which is where everything the host
 * keeps about the row lives.
 */
export function slotContextFor(row: { id: number; chatId: string }, w: SlotInputs, hasRecap = false): SlotContext {
  return {
    chatId: w.chatId,
    sessionId: row.chatId,
    isLatestAssistant: row.id === w.latestAssistantId,
    running: w.running,
    interactionOpen: w.interactionOpen,
    ...(hasRecap ? { hasRecap } : {}),
    ...(w.remoteDevice ? { remote: true } : {}),
  };
}

/**
 * Whether a message shows the pencil (edit and resend): a finished message of the session in the
 * window, with no turn running and no editor missing. Notes and page messages are not the user's
 * words to edit, and an earlier session is archived, which the host refuses to rewrite.
 */
export function canEditRow(
  row: { status: string; chatId: string },
  w: { chatId: string; running: boolean },
  has: { editor: boolean; note: boolean; page: boolean },
): boolean {
  return row.status === 'complete' && !w.running && !has.note && !has.page && has.editor && !isEarlierSession(row, w.chatId);
}
