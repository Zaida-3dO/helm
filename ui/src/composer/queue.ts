// The queue list's rules. Pure: given the pending submissions and the
// thread, says how each one is shown and which actions it offers.

import type { Capabilities, Submission } from '../api/types.ts';

export type QueueItemState = 'pending' | 'queued' | 'sending' | 'uncertain' | 'failed';

export interface QueueItemView {
  submission: Submission;
  /** The stable data-state hook value and the label's basis. */
  state: QueueItemState;
  label: string;
  /** The pill's colour family: amber waits, steer is purple, muted is a plain pending steer, error failed. */
  tone: QueueTone;
  actions: QueueActionName[];
}

export type QueueTone = 'amber' | 'steer' | 'muted' | 'error';

export type QueueActionName = 'withdraw' | 'steerNow' | 'sendNow' | 'recover' | 'move_up' | 'move_down';

export interface QueueContext {
  running: boolean;
  capabilities: Capabilities;
  showChoices: boolean;
  /** The send mode; a steer shows "Queued · Steer" in queue mode. */
  sendMode?: string;
}

/** In-flight steers first (in creation order), then queued follow-ups in queue order. */
export function orderPending(pending: Submission[]): Submission[] {
  const live = (s: Submission) => s.state !== 'withdrawn' && s.state !== 'superseded' && s.state !== 'delivered' && s.state !== 'complete';
  const steers = pending.filter((s) => live(s) && s.mode === 'steer').sort((a, b) => a.createdAt - b.createdAt);
  const rest = pending
    .filter((s) => live(s) && s.mode !== 'steer')
    .sort((a, b) => (a.position ?? Infinity) - (b.position ?? Infinity) || a.createdAt - b.createdAt);
  return [...steers, ...rest];
}

export function itemState(s: Submission): QueueItemState {
  if (s.state === 'uncertain') return 'uncertain';
  if (s.state === 'rejected') return 'failed';
  if (s.mode === 'steer' && (s.state === 'accepted' || s.state === 'saved')) return 'pending';
  if (s.state === 'dispatching' || s.state === 'saved') return 'sending';
  return 'queued';
}

const LABELS: Record<QueueItemState, string> = {
  pending: 'Pending',
  queued: 'Queued',
  sending: 'Sending',
  uncertain: 'Not sent',
  failed: 'Not delivered',
};

function toneOf(state: QueueItemState, steerQueued: boolean): QueueTone {
  if (state === 'pending') return steerQueued ? 'steer' : 'muted';
  return state === 'failed' ? 'error' : 'amber';
}

export function queueView(pending: Submission[], ctx: QueueContext): QueueItemView[] {
  const ordered = orderPending(pending);
  const queuedIds = ordered.filter((s) => itemState(s) === 'queued').map((s) => s.id);
  return ordered.map((s) => {
    const state = itemState(s);
    const actions: QueueActionName[] = [];
    if (state === 'pending' && s.state === 'accepted' && ctx.capabilities.withdraw) actions.push('withdraw');
    if (state === 'queued') {
      if (ctx.running && ctx.showChoices) actions.push('steerNow', 'sendNow');
      const i = queuedIds.indexOf(s.id);
      if (ctx.running && i > 0) actions.push('move_up');
      if (ctx.running && i >= 0 && i < queuedIds.length - 1) actions.push('move_down');
    }
    if (state === 'uncertain' || state === 'failed') actions.push('recover');
    const steerQueued = state === 'pending' && ctx.sendMode === 'queue';
    return { submission: s, state, label: steerQueued ? 'Queued · Steer' : LABELS[state], tone: toneOf(state, steerQueued), actions };
  });
}

/** The queue position a move lands on: the neighbour's position in the queued list. */
export function movePosition(pending: Submission[], id: string, delta: -1 | 1): number | null {
  const queued = orderPending(pending).filter((s) => itemState(s) === 'queued');
  const i = queued.findIndex((s) => s.id === id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= queued.length) return null;
  return queued[j]!.position ?? j;
}

/** The queue position a drop on another queued item lands on: that item's own position. */
export function dropPosition(pending: Submission[], id: string, overId: string): number | null {
  const queued = orderPending(pending).filter((s) => itemState(s) === 'queued');
  const from = queued.findIndex((s) => s.id === id);
  const to = queued.findIndex((s) => s.id === overId);
  if (from < 0 || to < 0 || from === to) return null;
  return queued[to]!.position ?? to;
}
