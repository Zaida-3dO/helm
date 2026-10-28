// The sidebar footer's Tasks entry. It shows only once a board address and token are saved, and
// opens the Tasks tab. The connection is looked at on mount, when a board sync reports in and when
// the window comes back into focus, so saving the connection in Settings makes the entry appear.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { IconBoard } from '../ui/icons.tsx';
import { toast } from '../ui/index.ts';
import { openTasks, tasksStoreFor } from './open.ts';

export function TasksEntry({ api, bus }: { api: Api; bus: EventBus }) {
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
  if (connection !== 'ready') return null;
  return (
    <button
      type="button"
      className="sb-foot"
      data-testid="nav-tasks"
      onClick={() => {
        if (!openTasks(api)) toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      }}
    >
      <IconBoard />
      <span>Tasks</span>
    </button>
  );
}
