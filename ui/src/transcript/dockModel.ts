// The activity dock's counts. Pure, so it is tested.

import type { MessageRow, ThreadState } from '../api/types.ts';
import { toolKind, toolStatus } from './toolKinds.ts';

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

/** What the settled line says about the chat's last turn. */
export interface FinishedTurn {
  /** From the turn's first output to its last, the span the "Worked for" row shows. */
  elapsedMs: number;
  /** The tool calls the turn made, in order. */
  tools: MessageRow[];
  /** How many of them changed files. */
  edits: number;
}

/**
 * The last turn of a chat, when it ended with a reply: the line the dock keeps once nothing runs.
 * A turn that is still going, failed or was stopped has nothing to settle, so there is no line.
 */
export function finishedTurn(rows: readonly MessageRow[]): FinishedTurn | null {
  let u = rows.length - 1;
  while (u >= 0 && !(rows[u]!.role === 'user' && rows[u]!.details?.mode !== 'steer')) u--;
  if (u < 0) return null;
  const work = rows.slice(u + 1);
  const failedOrStopped = work.some((r) => r.status === 'error' || r.status === 'interrupted' || r.status === 'streaming' || r.status === 'saved');
  if (work.length === 0 || failedOrStopped || !work.some((r) => r.role === 'assistant' && r.text.trim())) return null;
  const tools = work.filter((r) => r.role === 'toolResult');
  if (tools.some((r) => toolStatus(r) === 'running')) return null;
  return {
    elapsedMs: Math.max(0, work[work.length - 1]!.timestamp - work[0]!.timestamp),
    tools,
    edits: tools.filter((r) => toolKind(r.toolName) === 'edit').length,
  };
}

/** The settled line's count: "3 done", or "2 files edited" when the turn changed files. */
export function finishedCount(t: Pick<FinishedTurn, 'tools' | 'edits'>): string | null {
  if (t.edits > 0) return `${t.edits} ${t.edits === 1 ? 'file' : 'files'} edited`;
  return t.tools.length > 0 ? `${t.tools.length} done` : null;
}
