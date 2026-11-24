// The one status icon a row shows. Every row carries at most one, resolved from
// the row's own signals plus, for a collapsed parent, what rides up from its sub-chats.
// Priority: running, flagged, draft, done, then the quieter states, with unread last.
// A "done" row finished a reply the person has not opened yet (the attention summary);
// plain unread is a manual mark — the two get different icons, and done animates.

import type { ChatState, ChatStatus } from '../api/types.ts';
import type { RowAttention } from '../api/projects.ts';
import type { AttentionKind } from '../api/generated/AttentionKind.ts';
import { LOCAL_WORKING_LABEL, localWaitLabel } from '../state/localModel.ts';

export function isBusy(status: ChatStatus): boolean {
  return status === 'starting' || status === 'running' || status === 'stopping';
}

/** What the database row and live signals say about a chat, before the sidebar folds them
 *  into marks: used where a row is built (the local snapshot, the remote device list). */
export interface RowSignals {
  status: ChatStatus;
  unread: boolean;
  lingering: boolean;
  pinned: boolean;
}

export function rowAttention(s: RowSignals): RowAttention | null {
  if (isBusy(s.status)) return 'running';
  if (s.unread) return 'unread';
  if (s.lingering) return 'lingering';
  if (s.status === 'interrupted') return 'interrupted';
  if (s.pinned) return 'pinned';
  return null;
}

export const ATTENTION_LABEL: Record<RowAttention, string> = {
  running: 'Task running',
  unread: 'Unread result',
  lingering: 'Background work running',
  interrupted: 'Interrupted',
  pinned: 'Pinned',
};

export type RowStatus = 'running' | 'flagged' | 'draft' | 'done' | 'lingering' | 'interrupted' | 'pinned' | 'unread';

/** The signals one row folds into its status: the row's own, with counts already
 *  OR-ed in from its sub-chats when the parent is collapsed. */
export interface FoldSignals {
  running: boolean;
  flagged: boolean;
  draft: boolean;
  done: boolean;
  lingering: boolean;
  interrupted: boolean;
  pinned: boolean;
  unread: boolean;
}

/** The single status a row shows, or null when it is plain. */
export function foldStatus(s: FoldSignals): RowStatus | null {
  if (s.running) return 'running';
  if (s.flagged) return 'flagged';
  if (s.draft) return 'draft';
  if (s.done) return 'done';
  if (s.lingering) return 'lingering';
  if (s.interrupted) return 'interrupted';
  if (s.pinned) return 'pinned';
  if (s.unread) return 'unread';
  return null;
}

/** Relative age: "now" under a minute, then "Nm", "Nh", "Nd". */
export function age(updatedAt: number, now: number): string {
  const s = Math.max(0, Math.floor((now - updatedAt) / 1000));
  if (s < 60) return 'now';
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

export const STATUS_LABEL: Record<RowStatus, string> = {
  running: 'Task running',
  flagged: 'May need you',
  draft: 'Unsent draft',
  done: 'Finished, unread',
  lingering: 'Background work running',
  interrupted: 'Interrupted',
  pinned: 'Pinned',
  unread: 'Unread result',
};

/** The running mark's wording: a local chat says whether the model is queued behind other work
 * (`localAhead` set) or answering it now; a stalled Claude chat warns as before. */
export function rowStatusLabel(row: { stalled: boolean; local: boolean; localAhead: number | null }): string {
  if (row.local && row.localAhead !== null) return localWaitLabel(row.localAhead);
  if (row.stalled) return 'Running, no output for a while';
  if (row.local) return LOCAL_WORKING_LABEL;
  return ATTENTION_LABEL.running;
}

/** The running mark's state chip: short words for what the live state says, or null when the
 * wheel alone already says it (generating, or a card that raises its own flag). */
export function rowStateLabel(state: ChatState | null | undefined): string | null {
  if (!state) return null;
  switch (state.kind) {
    case 'queued':
      return state.ahead == null ? 'Queued' : `Queued ${state.ahead}`;
    case 'tool':
      return state.name;
    case 'silent':
      return 'Quiet';
    default:
      return null;
  }
}

/** The flag on a chat waiting on you: a question, plan or permission asks to be answered. */
export function needsYouLabel(kind: AttentionKind | undefined): string {
  return kind === 'question' || kind === 'plan' || kind === 'approval' ? 'Answer question' : 'Needs you';
}
