// The activity dock's counts. Pure, so it is tested.

import type { ThreadState } from '../api/types.ts';

/**
 * The background chip's tasks: shells only. Background subagents are counted once, by the
 * subagent roster chip beside it, never again as "background".
 */
export function backgroundShells(thread: Pick<ThreadState, 'backgroundTasks'>): ThreadState['backgroundTasks'] {
  return thread.backgroundTasks.filter((t) => t.kind !== 'agent');
}

/** The dock stays up while a turn runs, a background shell runs or a subagent works. */
export function dockShown(live: boolean, thread: Pick<ThreadState, 'backgroundTasks' | 'subagentsRunning'>): boolean {
  return live || backgroundShells(thread).length > 0 || (thread.subagentsRunning ?? 0) > 0;
}
