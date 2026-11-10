// The runtime check the setup card relies on: one shared answer per app run, so opening a chat
// does not start `node --version` and `claude --version` again. Only a definite "not found" blocks
// sending; a version probe that merely failed to answer is shown in Settings, not here. Until the
// first answer arrives (usually under a second) the composer is in its "starting" state.

import type { RuntimeStatus } from '../settings/runtime/model.ts';

export interface RuntimeCheck {
  /** Node or Claude Code could not be found at all. */
  blocked: boolean;
  /** Everything the check reported, for the card's details. */
  problems: string[];
  /** False until the first check has answered (or failed to). */
  checked: boolean;
}

/** Whether a status means a message could not run: a component with no path at all. */
export function isBlocked(s: Pick<RuntimeStatus, 'node' | 'claude'>): boolean {
  return !s.node.path || !s.claude.path;
}

/** How long the composer waits for the first answer before it stops holding Send back. */
const ANSWER_WAIT_MS = 5_000;

let state: RuntimeCheck = {blocked: false, problems: [], checked: false };
let asked = false;
let inflight: Promise<void> | null = null;
const listeners = new Set<() => void>();

export const runtimeCheck = {
  get: (): RuntimeCheck => state,
  subscribe(l: () => void): () => void {
    listeners.add(l);
    return () => listeners.delete(l);
  },
  /** Ask once per app run; later callers reuse the answer. */
  ensure(api: (op: string, args: unknown) => Promise<unknown>): void {
    if (!asked) void runtimeCheck.refresh(api);
  },
  /** Ask again (the retry button, the window coming back, Settings closing). Overlapping asks share one call. */
  refresh(api: (op: string, args: unknown) => Promise<unknown>): Promise<void> {
    asked = true;
    // No answer is not "not found": sending is allowed again, as it was before the check. This
    // covers a failed call and one that hangs; a late answer still replaces it.
    const giveUp = () => {
      if (!state.checked) {
        state = { ...state, checked: true };
        listeners.forEach((l) => l());
      }
    };
    const slow = inflight ? null : setTimeout(giveUp, ANSWER_WAIT_MS);
    inflight ??= api('runtime_status', {})
      .then((s) => {
        const status = s as RuntimeStatus;
        state = { blocked: isBlocked(status), problems: status.problems ?? [], checked: true };
        listeners.forEach((l) => l());
      })
      .catch(giveUp)
      .finally(() => {
        if (slow) clearTimeout(slow);
        inflight = null;
      });
    return inflight;
  },
};
