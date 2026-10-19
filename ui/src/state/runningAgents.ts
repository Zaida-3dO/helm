// Which Agent tool calls of a chat still have a running subagent, from the live thread state.
// The transcript keeps those calls outside the "worked for" fold until they end.

import { useMemo, useSyncExternalStore } from 'react';
import type { ThreadState } from '../api/types.ts';
import { useShell } from '../shell/context.ts';

/** The tool call ids of the thread's running subagents, as a stable comma-joined key. */
export function runningAgentKey(thread: ThreadState | null | undefined): string {
  const ids = (thread?.subagents ?? []).filter((s) => s.status === 'running').map((s) => s.toolCallId);
  return ids.sort().join(',');
}

/** The set behind a key from runningAgentKey. */
export function runningAgentSet(key: string): ReadonlySet<string> {
  return new Set(key ? key.split(',') : []);
}

/** The running Agent calls of a chat. The same set object until that list changes. */
export function useRunningAgents(chatId: string): ReadonlySet<string> {
  const store = useShell().store;
  const key = useSyncExternalStore(store.subscribe, () => runningAgentKey(store.view(chatId).thread));
  return useMemo(() => runningAgentSet(key), [key]);
}
