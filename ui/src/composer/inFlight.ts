// The in-flight list's rules. Pure: given the pending submissions and the thread, says how
// each one is shown and which actions it offers. Nothing here waits for a turn to end: a
// message is in flight until Claude has read it or it has been put back to the composer.

import type { Capabilities, MessageRow, Submission } from '../api/types.ts';
import { toolLabel, toolStatus } from '../transcript/toolKinds.ts';

export type InFlightState = 'pending' | 'sending' | 'uncertain' | 'failed';

export interface InFlightView {
  submission: Submission;
  /** The stable data-state hook value and the label's basis. */
  state: InFlightState;
  label: string;
  /** Why an unread message waits, in words ("Waiting for Bash to finish"); null for any other state. */
  reason: string | null;
  tone: InFlightTone;
  actions: InFlightAction[];
}

export type InFlightTone = 'muted' | 'amber' | 'error';

export type InFlightAction = 'withdraw' | 'recover';

export interface InFlightContext {
  capabilities: Capabilities;
  /** What an unread message is waiting behind (`waitReason`); defaults to the plain sentence. */
  reason?: string;
}

const NEXT_STEP = 'Claude reads it at its next step';

/**
 * Why a message Claude holds unread has not been read yet. Claude reads a message between steps,
 * so while a tool call is still running the message waits for it; otherwise it waits for the step
 * that is being written to end. Read from the live tool rows, newest first.
 */
export function waitReason(rows: readonly MessageRow[]): string {
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i]!;
    if (r.role === 'toolResult' && toolStatus(r) === 'running') return `Waiting for ${toolLabel(r.toolName)} to finish`;
    // The turn's own start: tools before it are history.
    if (r.role === 'user' && r.details?.mode !== 'steer') break;
  }
  return NEXT_STEP;
}

/** In flight in the order they were sent. The host's list already excludes settled rows. */
export function orderInFlight(pending: Submission[]): Submission[] {
  return pending.filter((s) => s.state !== 'withdrawn' && s.state !== 'superseded' && s.state !== 'delivered' && s.state !== 'complete').sort((a, b) => a.createdAt - b.createdAt);
}

export function itemState(s: Submission): InFlightState {
  if (s.state === 'uncertain') return 'uncertain';
  if (s.state === 'rejected') return 'failed';
  if (s.state === 'dispatching') return 'sending';
  // Saved or accepted: sent to Claude, not read by it yet.
  return 'pending';
}

const LABELS: Record<InFlightState, string> = {
  pending: 'Pending',
  sending: 'Sending',
  uncertain: 'Not sent',
  failed: 'Not delivered',
};

function toneOf(state: InFlightState): InFlightTone {
  if (state === 'pending') return 'muted';
  return state === 'failed' ? 'error' : 'amber';
}

export function inFlightView(pending: Submission[], ctx: InFlightContext): InFlightView[] {
  return orderInFlight(pending).map((s) => {
    const state = itemState(s);
    const actions: InFlightAction[] = [];
    // Withdraw only while Claude holds it unread; Recover only once it is known unsent.
    if (state === 'pending' && s.state === 'accepted' && ctx.capabilities.withdraw) actions.push('withdraw');
    if (state === 'uncertain' || state === 'failed') actions.push('recover');
    return { submission: s, state, label: LABELS[state], reason: state === 'pending' ? (ctx.reason ?? NEXT_STEP) : null, tone: toneOf(state), actions };
  });
}
