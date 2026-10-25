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
