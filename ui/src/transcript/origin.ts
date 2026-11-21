// Where a user message came from, when not Helm's composer. A Telegram message carries notes for
// the agent in front of its text ("answer briefly...", and what other chats sent the user since
// the last message); the bubble hides them and shows a Telegram badge instead. A message typed on
// another PC starts with a line naming that PC; the bubble hides it and says "from <PC>". The notes
// stay in the stored text, since the agent needs them.

import type { MessageRow } from '../api/types.ts';

/** The notes the host puts in front of a Telegram message (bracketed lines). */
const TELEGRAM_NOTE = /^\[Sent from Telegram:[^\]\n]*\]\n(\[Sent to the user on Telegram by other chats[^\]\n]*\]\n)?/;

/**
 * The notes the host puts in front of a voice message: the primer (first message of a
 * conversation, ending where the host's text does), the mode line, and a line for each reply the
 * user did not hear.
 */
const VOICE_NOTE = /^(\[Voice chat: you are[\s\S]*?If not, answer exactly \[quiet\]\.\]\n)?\[Voice · [^\]\n]*\]\n(\[Not heard by the user:[^\]\n]*\]\n)*/;

/**
 * A note Helm itself put in the chat for the agent, not something the user wrote or an agent sent:
 * the voice chat's `[Attention]` and `[Wait finished]` news. It is for the agent, so the
 * transcript does not draw it (the turn it starts still folds and settles as any other).
 */
export function isHelmNote(row: MessageRow): boolean {
  return row.role === 'user' && row.details?.origin === 'app' && /^\[(?:Attention|Wait finished)\]/.test(row.text);
}

/**
 * An agent's answer of `[quiet]`: what a voice chat says when a note needs no word to the user. It
 * is a signal for Helm, never drawn.
 */
export function isQuietReply(row: MessageRow): boolean {
  return row.role === 'assistant' && row.text.replace(/\s+/g, '').toLowerCase() === '[quiet]';
}

export function fromTelegram(row: MessageRow): boolean {
  return row.details?.origin === 'telegram';
}

export function fromVoice(row: MessageRow): boolean {
  return row.details?.origin === 'voice';
}

/**
 * The line the PC a chat runs on puts in front of the user's own words when they were typed on
 * another PC, so the agent knows where the person is: exactly `[Typed on <name>]` and a newline,
 * the name without brackets or control characters and at most 40 characters.
 */
// eslint-disable-next-line no-control-regex
const TYPED_ON = /^\[Typed on ([^[\]\u0000-\u001f\u007f]{1,40})\]\n/;

/** The PC a message from another PC was typed on, or null (typed here, or not that kind of message). */
export function typedOn(row: MessageRow): string | null {
  if (row.role !== 'user' || row.details?.origin !== 'remote') return null;
  const name = TYPED_ON.exec(row.text)?.[1]?.trim();
  return name ? name : null;
}

/** The text the bubble shows. */
export function bubbleText(row: MessageRow): string {
  if (fromVoice(row)) return row.text.replace(VOICE_NOTE, '');
  if (typedOn(row)) return row.text.replace(TYPED_ON, '');
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
