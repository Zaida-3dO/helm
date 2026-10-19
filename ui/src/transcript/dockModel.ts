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

export interface DockState {
  key: 'starting' | 'stopping' | 'compacting' | 'waiting' | 'working' | 'subagents' | 'background';
  /** Line 1's state. */
  label: string;
  /** What follows it, when there is something to say. */
  detail: string | null;
}

/** Line 1 of the dock: what the chat is doing now. */
export function dockState(
  status: string,
  thread: Pick<ThreadState, 'compacting' | 'interactions' | 'liveTools' | 'subagentsRunning'>,
): DockState {
  if (status === 'starting') return { key: 'starting', label: 'Starting agent', detail: 'Preparing your session' };
  if (status === 'stopping') return { key: 'stopping', label: 'Stopping safely', detail: null };
  if (status === 'running') {
    if (thread.compacting) return { key: 'compacting', label: 'Compacting conversation', detail: thread.compacting.retrying ? 'Retrying' : null };
    if (thread.interactions.length) return { key: 'waiting', label: 'Waiting for you', detail: 'Answer the card below to carry on' };
    const n = thread.subagentsRunning ?? 0;
    return { key: 'working', label: 'Working', detail: n > 0 ? `${n} subagent${n === 1 ? '' : 's'} working` : null };
  }
  if ((thread.subagentsRunning ?? 0) > 0) return { key: 'subagents', label: 'Subagents working', detail: 'The turn has ended; its agents carry on' };
  return { key: 'background', label: 'Background work', detail: 'The agent continues when each one exits' };
}
