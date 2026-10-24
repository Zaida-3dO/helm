// Where a user message came from, when not Helm's composer. A Telegram message carries a note
// for the agent in front of its text ("answer briefly..."); the bubble hides that note and shows
// a Telegram badge instead. The note stays in the stored text, since the agent needs it.

import type { MessageRow } from '../api/types.ts';

/** The note the host puts in front of every Telegram message (one bracketed line). */
const TELEGRAM_NOTE = /^\[Sent from Telegram:[^\]\n]*\]\n/;

export function fromTelegram(row: MessageRow): boolean {
  return row.details?.origin === 'telegram';
}

/** The text the bubble shows. */
export function bubbleText(row: MessageRow): string {
  return fromTelegram(row) ? row.text.replace(TELEGRAM_NOTE, '') : row.text;
}
