// The graphics-card hold as the composer sees it: a chat's note (set through the app_gpu tool) that
// the card is in use, so sending to a chat on a local model would reload the model server and spoil
// the work. It warns and never blocks. Pure, so it is tested without a window.

import type { LockView } from '../gpu/api.ts';

export interface GpuHold {
  reason: string;
  /** When it ends by itself, in ms since the epoch. */
  until: number;
}

/** The hold in force at `now`, or null: none set, or one that has run out. */
export function activeHold(view: Pick<LockView, 'holdReason' | 'holdUntil'> | null, now: number): GpuHold | null {
  if (!view || !view.holdReason || view.holdUntil === undefined || view.holdUntil <= now) return null;
  return { reason: view.holdReason, until: view.holdUntil };
}

/** The banner's line, and the question's sentence. */
export function holdSentence(hold: GpuHold): string {
  return `${hold.reason}: sending will reload the local model`;
}
