// Where a user message came from, when not Helm's composer. A Telegram message carries notes for
// the agent in front of its text ("answer briefly...", and what other chats sent the user since
// the last message); the bubble hides them and shows a Telegram badge instead. The notes stay in
// the stored text, since the agent needs them.

import type { MessageRow } from '../api/types.ts';

/** The notes the host puts in front of a Telegram message (bracketed lines). */
const TELEGRAM_NOTE = /^\[Sent from Telegram:[^\]\n]*\]\n(\[Sent to the user on Telegram by other chats[^\]\n]*\]\n)?/;

export function fromTelegram(row: MessageRow): boolean {
  return row.details?.origin === 'telegram';
}

/** The text the bubble shows. */
export function bubbleText(row: MessageRow): string {
  return fromTelegram(row) ? row.text.replace(TELEGRAM_NOTE, '') : row.text;
}

export interface PeerMessage {
  /** The sending chat's title as the host wrote it into the header. */
  title: string;
  chatId: string;
  /** What the sender wrote, without the header and the host's fixed paragraph. */
  body: string;
}

/** The header line, the host's one-paragraph note, a blank line, then the sender's text. */
const PEER_FRAME = /^\[Message from another chat: "([^"\n]*)" \(([^)\s]+)\)\]\n[^\n]*\n\n([\s\S]*)$/;

/**
 * A message another chat sent, split into sender and body for the card. Null for anything else,
 * and for an older or odd message whose text does not have the frame: that stays a plain bubble.
 */
export function peerMessage(row: MessageRow): PeerMessage | null {
  if (row.role !== 'user' || row.details?.origin !== 'other_chat') return null;
  const m = PEER_FRAME.exec(row.text);
  return m ? { title: (m[1] as string).trim() || 'Untitled', chatId: m[2] as string, body: m[3] as string } : null;
}
