// Composer chips: Continue, Yes and No, one click each.
//
// Nothing shows while the chat is running or an interaction card is open. Continue shows only when
// there is something to continue: the chat was stopped or interrupted, or its last row (not the
// user's) ended early or in an error. Yes and No show only when the last row is a complete
// assistant reply that ends in a question, so a normal finished reply shows no chip at all. What
// they send is fixed lowercase words, sent as an action (the draft is left alone).

import type { ChatStatus, MessageRow } from '../api/types.ts';

export interface QuickReply {
  id: 'continue' | 'yes' | 'no';
  label: string;
  /** The exact message sent. */
  text: string;
  /** What clicking does, for the tooltip and the accessible name (starts with the visible label). */
  hint: string;
}

export const CONTINUE: QuickReply = {
  id: 'continue',
  label: 'Continue',
  text: 'continue',
  hint: "Continue: sends 'continue' so Claude picks up where it stopped",
};
export const YES: QuickReply = { id: 'yes', label: 'Yes', text: 'yes', hint: "Yes: sends 'yes' as your answer" };
export const NO: QuickReply = { id: 'no', label: 'No', text: 'no', hint: "No: sends 'no' as your answer" };

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

/**
 * True when the work was cut short and "continue" would pick it up: the chat settled as stopped or
 * interrupted, or its last row (other than the user's own message) is interrupted or errored.
 */
export function stoppedEarly(ctx: Pick<QuickReplyContext, 'status' | 'rows'>): boolean {
  if (ctx.status === 'stopped' || ctx.status === 'interrupted') return true;
  const last = ctx.rows[ctx.rows.length - 1];
  return !!last && last.role !== 'user' && (last.status === 'interrupted' || last.status === 'error');
}

/** The chips to show now, in order; empty hides the strip. */
export function quickReplies(ctx: QuickReplyContext): QuickReply[] {
  if (ctx.interactionOpen || RUNNING.has(ctx.status)) return [];
  const chips: QuickReply[] = [];
  if (stoppedEarly(ctx)) chips.push(CONTINUE);
  const last = answerableReply(ctx);
  if (last && endsWithQuestion(last.text)) chips.push(YES, NO);
  return chips;
}
