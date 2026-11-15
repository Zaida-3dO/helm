// Opening Tasks, and the one store behind it. The store outlives the tab: closing the tab keeps what
// was read, and the next show reads again.

import type { Api } from '../api/client.ts';
import { openInPanel, panelStore } from '../panel/registry.ts';
import { BoardReader, remoteBoardOps } from './reader.ts';
import { TasksStore } from './store.ts';

export const TASKS_KIND = 'tasks';

/** One store per board: this PC's, plus one per mirrored chat whose board lives on the PC that
 * runs it. The stores outlive the tab, as the local one always did. */
const stores = new Map<string, TasksStore>();

/** The store behind Tasks. With a mirrored chat's id its reads go to the board set up on the
 * PC that runs that chat, and never carry that board's address or token. */
export function tasksStoreFor(api: Api, remoteChatId?: string | null): TasksStore {
  const key = remoteChatId ?? '';
  let store = stores.get(key);
  if (!store) {
    store = remoteChatId
      ? new TasksStore(
          api,
          Date.now,
          new BoardReader(remoteBoardOps(api, remoteChatId)),
          () => api<{ url: string; hasToken: boolean }>('devices_board_connection', { chatId: remoteChatId }),
        )
      : new TasksStore(api);
    stores.set(key, store);
  }
  return store;
}

/** The store the open Tasks tab is reading, for the header's Refresh and the tab's close. */
let shown: TasksStore | null = null;

export function shownTasksStore(api: Api): TasksStore {
  return shown ?? tasksStoreFor(api);
}

/** The Tasks view reports its store on show, so a tab restored at start-up gets the right one. */
export function noteTasksStore(store: TasksStore): void {
  shown = store;
}

/**
 * Open the Tasks tab, maximised. With an item id its drawer shows; without one any earlier drawer
 * is closed, so the sidebar entry always opens with nothing selected. With a mirrored chat's id
 * the tab reads that chat's PC's board. `false` means the panel refused (it has no room), and
 * the caller says so.
 */
export function openTasks(api: Api, itemId?: string, remoteChatId?: string | null): boolean {
  const store = tasksStoreFor(api, remoteChatId);
  shown = store;
  if (itemId) store.openDrawer(itemId);
  else store.closeDrawer();
  return openInPanel(TASKS_KIND, TASKS_KIND, { chatId: remoteChatId ?? '' }, { intent: 'user', chatId: null });
}

/** Leave maximised mode, so the chat or draft the user is sent to is not hidden behind the Tasks tab. */
export function restorePanel(): void {
  if (panelStore.model.maximized) panelStore.update((m) => m.setMaximized(false));
}
