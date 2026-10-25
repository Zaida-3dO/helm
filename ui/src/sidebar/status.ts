// The one mark a chat row shows, by priority: running > unread > lingering >
// interrupted > pinned. Unread comes from the attention summary and lingering
// from the live thread's background work; the host row carries what the database knows.

import type { ChatStatus } from '../api/types.ts';
import type { RowAttention } from '../api/projects.ts';
import type { AttentionKind } from '../api/generated/AttentionKind.ts';

export interface RowSignals {
  status: ChatStatus;
  unread: boolean;
  lingering: boolean;
  pinned: boolean;
}

export function isBusy(status: ChatStatus): boolean {
  return status === 'starting' || status === 'running' || status === 'stopping';
}

export function rowAttention(s: RowSignals): RowAttention | null {
  if (isBusy(s.status)) return 'running';
  if (s.unread) return 'unread';
  if (s.lingering) return 'lingering';
  if (s.status === 'interrupted') return 'interrupted';
  if (s.pinned) return 'pinned';
  return null;
}

/** Running, unread and lingering marks sit at the row's trailing edge. */
export function trailingIndicator(a: RowAttention | null): boolean {
  return a === 'running' || a === 'unread' || a === 'lingering';
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

export const ATTENTION_LABEL: Record<RowAttention, string> = {
  running: 'Task running',
  unread: 'Unread result',
  lingering: 'Background work running',
  interrupted: 'Interrupted',
  pinned: 'Pinned',
};

/** The flag on a chat waiting on you: a question, plan or permission asks to be answered. */
export function needsYouLabel(kind: AttentionKind | undefined): string {
  return kind === 'question' || kind === 'plan' || kind === 'approval' ? 'Answer question' : 'Needs you';
}
