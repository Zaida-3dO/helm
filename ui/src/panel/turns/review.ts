// Which finished turn the Changes view is asked to review. The turn card's Review button sets it;
// the Changes view shows that turn's recorded edits beside the working tree until another is set.

import { showSlot } from '../registry.ts';

export interface ReviewTarget {
  chatId: string;
  /** The turn's last assistant message, which is how the host names a turn. */
  messageId: number;
  /** Changes on every request, so asking for the same turn again switches back to it. */
  nonce: number;
}

let target: ReviewTarget | null = null;
let nonce = 0;
const listeners = new Set<() => void>();

export const reviewTarget = {
  get: (): ReviewTarget | null => target,
  subscribe: (l: () => void): (() => void) => {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
};

/** Opens the Changes slot on the recorded edits of one turn. */
export function openTurnReview(chatId: string, messageId: number): void {
  target = { chatId, messageId, nonce: ++nonce };
  listeners.forEach((l) => l());
  showSlot('changes');
}
