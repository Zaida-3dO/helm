// The runtime check the setup card relies on: one shared answer per app run, so opening a chat
// does not start `node --version` and `claude --version` again. Only a definite "not found" blocks
// sending; a version probe that merely failed to answer is shown in Settings, not here.

import type { RuntimeStatus } from '../settings/runtime/model.ts';

export interface RuntimeCheck {
  /** Node or Claude Code could not be found at all. */
  blocked: boolean;
  /** Everything the check reported, for the card's details. */
  problems: string[];
}

/** Whether a status means a message could not run: a component with no path at all. */
export function isBlocked(s: Pick<RuntimeStatus, 'node' | 'claude'>): boolean {
  return !s.node.path || !s.claude.path;
}

let state: RuntimeCheck = { blocked: false, problems: [] };
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
    inflight ??= api('runtime_status', {})
      .then((s) => {
        const status = s as RuntimeStatus;
        state = { blocked: isBlocked(status), problems: status.problems ?? [] };
        listeners.forEach((l) => l());
      })
      .catch(() => undefined)
      .finally(() => {
        inflight = null;
      });
    return inflight;
  },
};
