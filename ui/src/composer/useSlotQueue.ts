import { useEffect, useSyncExternalStore } from 'react';
import { useShell } from '../shell/context.ts';
import { slotQueueStoreFor } from './slotQueue.ts';

/** The shared run-slot queue view (null until the host has answered). */
export function useSlotQueue() {
  const shell = useShell();
  const store = slotQueueStoreFor(shell.api, shell.bus);
  useEffect(() => store.start(), [store]);
  const view = useSyncExternalStore(store.subscribe, () => store.snapshot);
  return { view, store };
}

/** Whether this chat has a send waiting for a free run slot. */
export function useSlotWaiting(chatId: string): boolean {
  const shell = useShell();
  const store = slotQueueStoreFor(shell.api, shell.bus);
  useEffect(() => store.start(), [store]);
  return useSyncExternalStore(store.subscribe, () => store.isWaiting(chatId));
}
