// Composer chips: Continue, Yes and No, one click each.
//
// They show only when the chat is not running, the last row is a complete assistant reply, and no
// interaction card is open. Yes and No show only when that reply ends in a question. What they send
// is fixed lowercase words, sent as an action (the draft is left alone).

import type { ChatStatus, MessageRow } from '../api/types.ts';

export interface QuickReply {
  id: 'continue' | 'yes' | 'no';
  label: string;
  /** The exact message sent. */
  text: string;
}

export const CONTINUE: QuickReply = { id: 'continue', label: 'Continue', text: 'continue' };
export const YES: QuickReply = { id: 'yes', label: 'Yes', text: 'yes' };
export const NO: QuickReply = { id: 'no', label: 'No', text: 'no' };

export interface QuickReplyContext {
  status: ChatStatus;
  rows: Pick<MessageRow, 'role' | 'status' | 'text'>[];
  /** A CLI question, approval or plan card is waiting on the user. */
  interactionOpen: boolean;
}

const RUNNING: ReadonlySet<ChatStatus> = new Set(['starting', 'running', 'stopping']);

/** True when the text's last sentence is a question. Trailing markdown and quotes are ignored. */
export function endsWithQuestion(text: string): boolean {
  const t = text.trimEnd().replace(/[\s*_`"'”’)\]>]+$/u, '');
  return t.endsWith('?');
}

/**
 * The reply the user can answer with one click: the chat's last row, when it is a complete
 * assistant reply, nothing is running and no interaction card is open. Chips and option buttons
 * both hang off it.
 */
export function answerableReply<R extends Pick<MessageRow, 'role' | 'status' | 'text'>>(
  ctx: Omit<QuickReplyContext, 'rows'> & { rows: R[] },
): R | null {
  if (ctx.interactionOpen || RUNNING.has(ctx.status)) return null;
  const last = ctx.rows[ctx.rows.length - 1];
  if (!last || last.role !== 'assistant' || last.status !== 'complete' || last.text.trim() === '') return null;
  return last;
}

/** The chips to show now, in order; empty hides the strip. */
export function quickReplies(ctx: QuickReplyContext): QuickReply[] {
  const last = answerableReply(ctx);
  if (!last) return [];
  return endsWithQuestion(last.text) ? [CONTINUE, YES, NO] : [CONTINUE];
}
