// The sidebar's Tasks view. It shows only once a board address and token are saved, and opens the
// Tasks tab. The connection is looked at on mount, when a board sync reports in and when the
// window comes back into focus, so saving the connection in Settings makes the entry appear.
// `useTasksEntry` is what the sidebar's tile row and its More menu share.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { toast } from '../ui/index.ts';
import { openTasks, tasksStoreFor } from './open.ts';

/** Whether the board is connected, and how to open the Tasks tab. */
export function useTasksEntry(api: Api, bus: EventBus): { ready: boolean; open: () => void } {
  const store = tasksStoreFor(api);
  const connection = useSyncExternalStore(store.subscribe, () => store.snapshot().connection);
  useEffect(() => {
    const check = () => void store.checkConnection();
    check();
    const off = bus.on('board_sync', check);
    window.addEventListener('focus', check);
    return () => {
      off();
      window.removeEventListener('focus', check);
    };
  }, [store, bus]);
  return {
    ready: connection === 'ready',
    open: () => {
      if (!openTasks(api)) toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
    },
  };
}
