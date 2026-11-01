// The in-flight list's rules. Pure: given the pending submissions and the thread, says how
// each one is shown and which actions it offers. Nothing here waits for a turn to end: a
// message is in flight until Claude has read it or it has been put back to the composer.

import type { Capabilities, Submission } from '../api/types.ts';

export type InFlightState = 'pending' | 'sending' | 'uncertain' | 'failed';

export interface InFlightView {
  submission: Submission;
  /** The stable data-state hook value and the label's basis. */
  state: InFlightState;
  label: string;
  tone: InFlightTone;
  actions: InFlightAction[];
}

export type InFlightTone = 'muted' | 'amber' | 'error';

export type InFlightAction = 'withdraw' | 'recover';

export interface InFlightContext {
  capabilities: Capabilities;
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
    return { submission: s, state, label: LABELS[state], tone: toneOf(state), actions };
  });
}
