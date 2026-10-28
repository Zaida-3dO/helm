// Opening Tasks, and the one store behind it. The store outlives the tab: closing the tab keeps what
// was read, and the next show reads again.

import type { Api } from '../api/client.ts';
import { openInPanel, panelStore } from '../panel/registry.ts';
import { TasksStore } from './store.ts';

export const TASKS_KIND = 'tasks';

let shared: TasksStore | null = null;

export function tasksStoreFor(api: Api): TasksStore {
  shared ??= new TasksStore(api);
  return shared;
}

/**
 * Open the Tasks tab, maximised. With an item id its drawer shows; without one any earlier drawer
 * is closed, so the sidebar entry always opens with nothing selected. `false` means the panel
 * refused (it has no room), and the caller says so.
 */
export function openTasks(api: Api, itemId?: string): boolean {
  const store = tasksStoreFor(api);
  if (itemId) store.openDrawer(itemId);
  else store.closeDrawer();
  return openInPanel(TASKS_KIND, TASKS_KIND, {}, { intent: 'user', chatId: null });
}

/** Leave maximised mode, so the chat or draft the user is sent to is not hidden behind the Tasks tab. */
export function restorePanel(): void {
  if (panelStore.model.maximized) panelStore.update((m) => m.setMaximized(false));
}
